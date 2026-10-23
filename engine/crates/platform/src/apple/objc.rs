//! What the Apple modules need of the Objective-C runtime and CoreFoundation,
//! driven from Rust: messages sent through each method's own prototype,
//! references held and released, CF values made for dictionaries a framework
//! reads, and blocks for the completion handlers it calls.

use std::ffi::{CStr, c_char, c_void};

pub(crate) type Id = *mut c_void;
pub(crate) type Sel = *const c_void;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct CMTime {
    pub value: i64,
    pub timescale: i32,
    pub flags: u32,
    pub epoch: i64,
}

/// `CFDictionaryKeyCallBacks` / `CFDictionaryValueCallBacks`, only ever passed
/// by address.
#[repr(C)]
pub(crate) struct CallBacks([usize; 6]);

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Sel;
    pub(crate) fn objc_msgSend();
    fn objc_retain(object: Id) -> Id;
    fn objc_release(object: Id);
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithBytes(
        allocator: *const c_void,
        bytes: *const u8,
        length: isize,
        encoding: u32,
        external: u8,
    ) -> *const c_void;
    fn CFNumberCreate(allocator: *const c_void, kind: isize, value: *const c_void)
    -> *const c_void;
    fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        count: isize,
        key_callbacks: *const CallBacks,
        value_callbacks: *const CallBacks,
    ) -> *const c_void;
    pub(crate) fn CFRelease(object: *const c_void);
    static kCFTypeDictionaryKeyCallBacks: CallBacks;
    static kCFTypeDictionaryValueCallBacks: CallBacks;
    pub(crate) static kCFBooleanTrue: *const c_void;
}

// `_NSConcreteStackBlock`, the class of a block made on the stack: libSystem's.
unsafe extern "C" {
    static _NSConcreteStackBlock: c_void;
}

const UTF8: u32 = 0x0800_0100;
const SINT32: isize = 3;
const SINT64: isize = 4;

pub(crate) fn sel(name: &CStr) -> Sel {
    // SAFETY: registering a selector has no preconditions.
    unsafe { sel_registerName(name.as_ptr()) }
}

pub(crate) fn class(name: &CStr) -> Id {
    // SAFETY: looking a class up has no preconditions.
    unsafe { objc_getClass(name.as_ptr()) }
}

/// `objc_msgSend` through the prototype of the method it dispatches to: every
/// call names the method's own argument and return types. No call returns a
/// structure, which on x86_64 would need `objc_msgSend_stret`.
macro_rules! send {
    ($receiver:expr, $selector:expr => $ret:ty) => {{
        let call: unsafe extern "C" fn($crate::apple::objc::Id, $crate::apple::objc::Sel) -> $ret =
            std::mem::transmute($crate::apple::objc::objc_msgSend as *const ());
        call($receiver, $selector)
    }};
    ($receiver:expr, $selector:expr, $($arg:expr => $ty:ty),+ ; $ret:ty) => {{
        let call: unsafe extern "C" fn($crate::apple::objc::Id, $crate::apple::objc::Sel, $($ty),+) -> $ret =
            std::mem::transmute($crate::apple::objc::objc_msgSend as *const ());
        call($receiver, $selector, $($arg),+)
    }};
}
pub(crate) use send;

/// An autorelease pool for the length of one call on a thread that has none of
/// its own.
pub(crate) struct Pool(*mut c_void);

impl Pool {
    pub(crate) fn new() -> Self {
        // SAFETY: pushing a pool has no preconditions; it is popped on drop.
        Self(unsafe { objc_autoreleasePoolPush() })
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        // SAFETY: the pool pushed by `new`, popped once.
        unsafe { objc_autoreleasePoolPop(self.0) }
    }
}

/// An object this module holds a reference to.
pub(crate) struct Owned(pub(crate) Id);

impl Owned {
    /// Take a reference to an object the caller does not own.
    pub(crate) fn retain(object: Id) -> Option<Self> {
        // SAFETY: a non-null object is live for the call that returned it.
        (!object.is_null()).then(|| Self(unsafe { objc_retain(object) }))
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: the reference `retain` took.
        unsafe { objc_release(self.0) }
    }
}

/// A CoreFoundation value this module made, released when dropped. CF strings,
/// numbers and dictionaries are their Foundation counterparts too, so one is
/// also what a method taking an `NSString` or `NSDictionary` is given.
pub(crate) struct Cf(pub(crate) *const c_void);

impl Cf {
    pub(crate) fn string(text: &str) -> Option<Self> {
        // SAFETY: the bytes are valid UTF-8 for their length.
        let made = unsafe {
            CFStringCreateWithBytes(
                std::ptr::null(),
                text.as_ptr(),
                text.len() as isize,
                UTF8,
                0,
            )
        };
        (!made.is_null()).then_some(Self(made))
    }

    pub(crate) fn i32(value: i32) -> Self {
        // SAFETY: a number made from a value on the stack.
        Self(unsafe { CFNumberCreate(std::ptr::null(), SINT32, (&raw const value).cast()) })
    }

    pub(crate) fn i64(value: i64) -> Self {
        // SAFETY: as `i32`.
        Self(unsafe { CFNumberCreate(std::ptr::null(), SINT64, (&raw const value).cast()) })
    }

    /// A dictionary of `entries`, each key a CF string constant a framework
    /// exports; the dictionary retains what it holds.
    pub(crate) fn dictionary(entries: &[(*const c_void, *const c_void)]) -> Self {
        let keys: Vec<_> = entries.iter().map(|(key, _)| *key).collect();
        let values: Vec<_> = entries.iter().map(|(_, value)| *value).collect();
        // SAFETY: two arrays of `entries.len()` CF objects, with CF's own
        // retaining callbacks.
        Self(unsafe {
            CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                entries.len() as isize,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            )
        })
    }

    pub(crate) fn id(&self) -> Id {
        self.0.cast_mut()
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: a value this module made, released once.
            unsafe { CFRelease(self.0) }
        }
    }
}

/// What failed, with the system's reason when it gave one: `error` is an
/// `NSError` or nil.
///
/// # Safety
///
/// `error` is nil or a live `NSError`.
pub(crate) unsafe fn describe(what: &str, error: Id) -> String {
    if error.is_null() {
        return format!("{what} failed");
    }
    // SAFETY: an NSError's description is an NSString with a UTF-8 rendering.
    unsafe {
        let text: Id = send!(error, sel(c"localizedDescription") => Id);
        let utf8: *const c_char = if text.is_null() {
            std::ptr::null()
        } else {
            send!(text, sel(c"UTF8String") => *const c_char)
        };
        if utf8.is_null() {
            format!("{what} failed")
        } else {
            format!("{what} failed: {}", CStr::from_ptr(utf8).to_string_lossy())
        }
    }
}

/// A block taking no arguments that calls `run` with `context` when invoked,
/// for an API whose completion handler is `void (^)(void)`. Built on the stack
/// as clang builds a block literal (the Block ABI's layout); the API copies it,
/// and the copy carries `context` by value -- so `context` must be something
/// the callback may own, such as a pointer from `Arc::into_raw`.
#[repr(C)]
pub(crate) struct Block<C: Copy> {
    isa: *const c_void,
    flags: i32,
    reserved: i32,
    invoke: unsafe extern "C" fn(*const Block<C>),
    descriptor: *const BlockDescriptor,
    pub(crate) context: C,
}

#[repr(C)]
pub(crate) struct BlockDescriptor {
    reserved: usize,
    size: usize,
    signature: *const c_char,
}

// SAFETY: the descriptor is immutable and its signature a static C string.
unsafe impl Sync for BlockDescriptor {}

/// `BLOCK_HAS_SIGNATURE`: the descriptor says the block's type encoding.
const BLOCK_HAS_SIGNATURE: i32 = 1 << 30;

impl<C: Copy> Block<C> {
    /// A block for `descriptor`, which must be [`Block::descriptor`] of this `C`.
    pub(crate) fn new(
        context: C,
        invoke: unsafe extern "C" fn(*const Block<C>),
        descriptor: &'static BlockDescriptor,
    ) -> Self {
        Self {
            isa: &raw const _NSConcreteStackBlock,
            flags: BLOCK_HAS_SIGNATURE,
            reserved: 0,
            invoke,
            descriptor,
            context,
        }
    }

    /// The descriptor of a `void (^)(void)` block carrying a `C`.
    pub(crate) const fn descriptor() -> BlockDescriptor {
        BlockDescriptor {
            reserved: 0,
            size: std::mem::size_of::<Self>(),
            signature: c"v8@?0".as_ptr(),
        }
    }
}
