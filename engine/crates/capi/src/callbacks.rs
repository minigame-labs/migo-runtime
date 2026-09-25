//! Host callbacks: engine events delivered to C through the host's dispatcher.
//!
//! The headers are strict about how this works, and the rules exist for
//! reasons that show up as crashes when ignored:
//!
//! * a non-null callback requires a non-null dispatcher — engine events arrive
//!   on the render or host thread, and delivering them there would run host
//!   code on a thread it never agreed to;
//! * callbacks install once, before the first attach, so a queued task can
//!   never observe a replaced function pointer or `user_data`;
//! * a task handed to the dispatcher must run exactly once, and it owns its
//!   payload until it does.

use std::{
    ffi::{CString, c_void},
    os::raw::c_char,
    sync::{Arc, Weak},
};

use crate::MigoSession;
use migo_capi_abi::{MIGO_OK, MigoResult, VersionedHeader};

// Some aliases are currently referenced only by host-facing tests, but keeping
// the complete callback vocabulary together prevents signature drift.
#[allow(unused_imports)]
pub use migo_capi_abi::callbacks::{
    MigoDispatchFn, MigoError, MigoHostCallbacks, MigoKeyboardShowOptions, MigoOnErrorFn,
    MigoOnExitRequestedFn, MigoOnHideKeyboardFn, MigoOnReadyFn, MigoOnRequestFrameFn,
    MigoOnShowKeyboardFn, MigoOnSurfaceLostFn, MigoOnSurfaceReleasedFn, MigoOnUpdateKeyboardFn,
    MigoTaskFn, ValidatedHostCallbacks as HostCallbacks,
};

/// `MIGO_KEYBOARD_FLAG_*` from `include/migo/session.h`.
pub const MIGO_KEYBOARD_FLAG_NONE: u32 = 0;
pub const MIGO_KEYBOARD_FLAG_MULTIPLE: u32 = 1 << 0;
pub const MIGO_KEYBOARD_FLAG_CONFIRM_HOLD: u32 = 1 << 1;

/// `MIGO_KEYBOARD_CONFIRM_*`, in the common mini-game platform's order.
pub const MIGO_KEYBOARD_CONFIRM_DONE: u32 = 0;
pub const MIGO_KEYBOARD_CONFIRM_NEXT: u32 = 1;
pub const MIGO_KEYBOARD_CONFIRM_SEARCH: u32 = 2;
pub const MIGO_KEYBOARD_CONFIRM_GO: u32 = 3;
pub const MIGO_KEYBOARD_CONFIRM_SEND: u32 = 4;

/// `MIGO_KEYBOARD_TYPE_*`.
pub const MIGO_KEYBOARD_TYPE_TEXT: u32 = 0;
pub const MIGO_KEYBOARD_TYPE_NUMBER: u32 = 1;

/// The engine-side owned form, which a queued task holds until it runs.
///
/// The C struct borrows a pointer; a task that outlives the call that created
/// it cannot, so the two shapes are deliberately different.
#[derive(Default, Clone)]
pub struct ShowOptions {
    pub flags: u32,
    pub max_length: u32,
    pub confirm_type: u32,
    pub keyboard_type: u32,
    pub default_value: String,
}

/// What a queued task should invoke once the dispatcher runs it.
enum Event {
    Ready,
    ExitRequested,
    Error { code: MigoResult, message: CString },
    SurfaceLost { generation: u64, reason: u32 },
    SurfaceReleased { generation: u64 },
    RequestFrame,
    ShowKeyboard { options: ShowOptions },
    HideKeyboard,
    UpdateKeyboard { value: String },
    Vibrate { vibration: u32 },
    KeepScreenOn { keep_on: bool },
    GameLog { entry_json: String },
}

/// Payload owned by a dispatched task until it runs.
struct Task {
    callbacks: HostCallbacks,
    /// Pins the public allocation across the complete callback, including a
    /// reentrant successful `migo_session_destroy` from within that callback.
    session: Arc<MigoSession>,
    event: Event,
}

/// Invoked by the host's dispatcher. Consumes the payload exactly once.
///
/// # Safety
/// `context` must be the pointer produced by [`Notifier::post`].
unsafe extern "C" fn run_task(context: *mut c_void) {
    if context.is_null() {
        return;
    }
    let task = unsafe { Box::from_raw(context as *mut Task) };
    // Entry and destruction share one atomic linearization gate. The Task's Arc
    // keeps the allocation alive, while the permit makes destroy wait if this
    // callback won entry on another thread.
    let Some(permit) = task.session.callback_gate.try_enter() else {
        return;
    };
    permit.run(|| invoke_task(&task));
}

/// Invoke one user callback after [`run_task`] has acquired callback admission.
fn invoke_task(task: &Task) {
    let user_data = task.callbacks.user_data;
    let session = Arc::as_ptr(&task.session).cast_mut().cast::<c_void>();
    match &task.event {
        Event::Ready => {
            if let Some(on_ready) = task.callbacks.on_ready {
                unsafe { on_ready(user_data, session) };
            }
        }
        Event::ExitRequested => {
            if let Some(on_exit) = task.callbacks.on_exit_requested {
                unsafe { on_exit(user_data, session) };
            }
        }
        Event::Error { code, message } => {
            if let Some(on_error) = task.callbacks.on_error {
                let bytes = message.as_bytes();
                let error = MigoError {
                    header: VersionedHeader {
                        struct_size: size_of::<MigoError>() as u32,
                        abi_version: migo_capi_abi::MIGO_ABI_VERSION_CURRENT,
                    },
                    code: *code,
                    flags: 0,
                    message_utf8: message.as_ptr(),
                    message_length: bytes.len() as u32,
                    reserved0: 0,
                };
                // `message` outlives the call because `task` is dropped after.
                unsafe { on_error(user_data, session, &error) };
            }
        }
        Event::SurfaceLost { generation, reason } => {
            if let Some(on_surface_lost) = task.callbacks.on_surface_lost {
                unsafe { on_surface_lost(user_data, session, *generation, *reason) };
            }
        }
        Event::SurfaceReleased { generation } => {
            if let Some(on_surface_released) = task.callbacks.on_surface_released {
                unsafe { on_surface_released(user_data, session, *generation) };
            }
        }
        Event::RequestFrame => {
            if let Some(on_request_frame) = task.callbacks.on_request_frame {
                unsafe { on_request_frame(user_data, session) };
            }
        }
        Event::ShowKeyboard { options } => {
            if let Some(on_show_keyboard) = task.callbacks.on_show_keyboard {
                let default_value = options.default_value.as_bytes();
                let c_options = MigoKeyboardShowOptions {
                    header: VersionedHeader {
                        struct_size: size_of::<MigoKeyboardShowOptions>() as u32,
                        abi_version: migo_capi_abi::MIGO_ABI_VERSION_CURRENT,
                    },
                    flags: options.flags,
                    max_length: options.max_length,
                    confirm_type: options.confirm_type,
                    keyboard_type: options.keyboard_type,
                    default_value_utf8: default_value.as_ptr() as *const c_char,
                    default_value_length: default_value.len() as u32,
                    reserved0: 0,
                };
                // `options` outlives the call because `task` is dropped after.
                unsafe { on_show_keyboard(user_data, session, &c_options) };
            }
        }
        Event::HideKeyboard => {
            if let Some(on_hide_keyboard) = task.callbacks.on_hide_keyboard {
                unsafe { on_hide_keyboard(user_data, session) };
            }
        }
        Event::UpdateKeyboard { value } => {
            if let Some(on_update_keyboard) = task.callbacks.on_update_keyboard {
                let bytes = value.as_bytes();
                unsafe {
                    on_update_keyboard(
                        user_data,
                        session,
                        bytes.as_ptr() as *const c_char,
                        bytes.len() as u32,
                    )
                };
            }
        }
        Event::Vibrate { vibration } => {
            if let Some(on_vibrate) = task.callbacks.on_vibrate {
                unsafe { on_vibrate(user_data, session, *vibration) };
            }
        }
        Event::KeepScreenOn { keep_on } => {
            if let Some(on_keep_screen_on) = task.callbacks.on_keep_screen_on {
                unsafe { on_keep_screen_on(user_data, session, u8::from(*keep_on)) };
            }
        }
        Event::GameLog { entry_json } => {
            if let Some(on_game_log) = task.callbacks.on_game_log {
                let bytes = entry_json.as_bytes();
                unsafe {
                    on_game_log(
                        user_data,
                        session,
                        bytes.as_ptr() as *const c_char,
                        bytes.len() as u32,
                    )
                };
            }
        }
    }
}

/// Routes engine notifications to the host's callbacks.
pub struct Notifier {
    callbacks: HostCallbacks,
    session: Weak<MigoSession>,
}

impl Notifier {
    pub fn new(callbacks: HostCallbacks, session: Weak<MigoSession>) -> Self {
        Self { callbacks, session }
    }

    /// Hand one event to the host's dispatcher.
    ///
    /// If the dispatcher refuses the task, ownership stays here and the payload
    /// is dropped — the header's rule that a rejected dispatch leaves the task
    /// with Migo.
    ///
    /// Returns whether the dispatcher accepted the task, which is as much as
    /// this side can know: acceptance is not evidence the host has acted.
    /// Notifications discard it, but the keyboard verbs answer content with it,
    /// because content that believes its keyboard is opening waits for input
    /// that will never arrive.
    fn post(&self, event: Event) -> bool {
        let Some(session) = self.session.upgrade() else {
            return false;
        };
        // `dispatch` is itself a call into host code and reads
        // `dispatcher_data`. It therefore shares the same lifetime boundary as
        // the eventual callback. Acquiring before materialising the task makes
        // Session destruction wait for a dispatcher already in flight, while
        // a dispatcher that has not entered before close is rejected without
        // touching host-owned memory.
        let Some(dispatch_permit) = session.callback_gate.try_enter() else {
            return false;
        };
        let task = Box::new(Task {
            callbacks: self.callbacks,
            // The stack-owned Arc above pins the gate until `dispatch`
            // returns. The task needs a distinct pin because a dispatcher may
            // either queue it or execute it synchronously; in the latter case
            // a reentrant callback can destroy the public Session before this
            // outer dispatch permit is dropped.
            session: Arc::clone(&session),
            event,
        });
        let context = Box::into_raw(task) as *mut c_void;
        // SAFETY: `dispatch` came from the host and is called with the token it
        // supplied plus a task it must run exactly once.
        let result = dispatch_permit.run(|| unsafe {
            (self.callbacks.dispatch)(self.callbacks.dispatcher_data, run_task, context)
        });
        if result != MIGO_OK {
            // Rejected: take the payload back and drop it, so the task neither
            // leaks nor runs.
            drop(unsafe { Box::from_raw(context as *mut Task) });
            tracing::warn!("host dispatcher rejected a callback task: {result}");
            return false;
        }
        true
    }

    pub fn ready(&self) {
        let _ = self.post(Event::Ready);
    }

    pub fn exit_requested(&self) {
        let _ = self.post(Event::ExitRequested);
    }

    pub fn error(&self, code: MigoResult, message: impl Into<Vec<u8>>) {
        // Interior NULs cannot travel through a C string; replace rather than
        // drop the report, since an error is exactly when detail matters.
        let message = CString::new(message).unwrap_or_else(|_| {
            CString::new("engine error (message contained an interior NUL)").expect("static")
        });
        let _ = self.post(Event::Error { code, message });
    }

    #[allow(dead_code)]
    /// Ask the host to schedule one frame. Silent when the host installed no
    /// frame callback: that host is engine-paced and never asked to be told.
    pub fn request_frame(&self) {
        if self.callbacks.on_request_frame.is_some() {
            let _ = self.post(Event::RequestFrame);
        }
    }

    /// Ask the host to show, hide or update its keyboard.
    ///
    /// The returned flag means the dispatcher accepted the task, not that the
    /// host has done anything: the ABI is asynchronous and cannot promise more.
    /// A refusal has to reach content as a failure.
    pub fn show_keyboard(&self, options: ShowOptions) -> bool {
        self.post(Event::ShowKeyboard { options })
    }

    pub fn hide_keyboard(&self) -> bool {
        self.post(Event::HideKeyboard)
    }

    pub fn update_keyboard(&self, value: String) -> bool {
        self.post(Event::UpdateKeyboard { value })
    }

    /// Ask the host to vibrate, hold the display awake, or keep a log entry.
    /// Like the keyboard verbs, the flag is the dispatcher's acceptance.
    pub fn vibrate(&self, vibration: u32) -> bool {
        self.post(Event::Vibrate { vibration })
    }

    pub fn keep_screen_on(&self, keep_on: bool) -> bool {
        self.post(Event::KeepScreenOn { keep_on })
    }

    pub fn game_log(&self, entry_json: String) -> bool {
        self.post(Event::GameLog { entry_json })
    }

    #[inline]
    pub fn supplies_vibration(&self) -> bool {
        self.callbacks.on_vibrate.is_some()
    }

    #[inline]
    pub fn supplies_keep_screen_on(&self) -> bool {
        self.callbacks.on_keep_screen_on.is_some()
    }

    #[inline]
    pub fn supplies_game_log(&self) -> bool {
        self.callbacks.on_game_log.is_some()
    }

    /// Whether the host offered to service the soft keyboard, which is what
    /// decides whether the capability is offered to content at all — the same
    /// rule `drives_frames` follows for the frame clock.
    pub fn supplies_keyboard(&self) -> bool {
        self.callbacks.supplies_keyboard()
    }

    /// Whether the host offered to drive frames, which is the honest answer to
    /// `FrameClock::uses_external_vsync` rather than a fixed one.
    pub fn drives_frames(&self) -> bool {
        self.callbacks.on_request_frame.is_some()
    }

    #[inline]
    pub fn notifies_surface_release(&self) -> bool {
        self.callbacks.on_surface_released.is_some()
    }

    pub fn surface_lost(&self, generation: u64, reason: u32) {
        let _ = self.post(Event::SurfaceLost { generation, reason });
    }

    /// Wake a host that may now query a stored release object. The callback is
    /// deliberately not the source of truth and may be rejected or canceled.
    pub fn surface_released(&self, generation: u64) {
        if self.callbacks.on_surface_released.is_some() {
            let _ = self.post(Event::SurfaceReleased { generation });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        migo_session_create, migo_session_destroy,
        test_support::{callback_session_pin, session_config, with_engine},
    };
    use migo_capi_abi::MIGO_ERROR_INVALID_ARGUMENT;
    use std::{
        ops::Deref,
        sync::{
            Condvar, Mutex,
            atomic::{AtomicI32, AtomicUsize, Ordering},
            mpsc,
        },
        time::Duration,
    };

    struct TestNotifier {
        notifier: Notifier,
        session: Arc<MigoSession>,
    }

    impl Deref for TestNotifier {
        type Target = Notifier;

        fn deref(&self) -> &Self::Target {
            &self.notifier
        }
    }

    fn test_notifier(callbacks: HostCallbacks) -> TestNotifier {
        let session = callback_session_pin();
        let notifier = Notifier::new(callbacks, Arc::downgrade(&session));
        TestNotifier { notifier, session }
    }

    // Everything a C callback in this module records has to be process-global:
    // the callbacks are `extern "C"` and are invoked, on the test's own thread,
    // from inside the code the test is exercising, so there is nowhere to thread
    // a value out through. Atomics rather than a `Mutex<Counters>` for the same
    // reason -- the writer runs while the test holds the lock below, and taking
    // it again there would deadlock.
    static READY_CALLS: AtomicUsize = AtomicUsize::new(0);
    static DISPATCH_CALLS: AtomicUsize = AtomicUsize::new(0);
    static LAST_ERROR_CODE: AtomicUsize = AtomicUsize::new(0);
    static DESTROY_RESULT: AtomicI32 = AtomicI32::new(i32::MIN);

    /// Held by every test that resets one of the statics above and then reads
    /// it. Cargo runs a crate's tests in parallel threads of one process, so
    /// without it one test's reset lands between another's write and its read,
    /// and the second reads the value the first cleared.
    ///
    /// The rule is phrased that way, and this declaration sits below all four
    /// statics, because the previous wording was positional -- "the counters
    /// above" -- while `LAST_ERROR_CODE` and `DESTROY_RESULT` were declared
    /// *below* it. Both were added later, both were reset and read without the
    /// lock, and a comment that correctly described the hazard was describing a
    /// set that no longer matched the code. It surfaced as
    /// `interior_nul_messages_still_report_the_error` reading 0 where it had
    /// just written 1, in CI, on a change that touched none of this -- and
    /// passing every local run, three times over, because the race needs the
    /// two tests to interleave.
    static COUNTER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static QUEUED_TASK: std::sync::Mutex<Option<(MigoTaskFn, usize)>> = std::sync::Mutex::new(None);

    unsafe extern "C" fn inline_dispatch(
        _dispatcher: *mut c_void,
        task: MigoTaskFn,
        context: *mut c_void,
    ) -> MigoResult {
        unsafe { task(context) };
        MIGO_OK
    }

    unsafe extern "C" fn counting_inline_dispatch(
        dispatcher: *mut c_void,
        task: MigoTaskFn,
        context: *mut c_void,
    ) -> MigoResult {
        DISPATCH_CALLS.fetch_add(1, Ordering::SeqCst);
        unsafe { inline_dispatch(dispatcher, task, context) }
    }

    unsafe extern "C" fn rejecting_dispatch(
        _dispatcher: *mut c_void,
        _task: MigoTaskFn,
        _context: *mut c_void,
    ) -> MigoResult {
        MIGO_ERROR_INVALID_ARGUMENT
    }

    unsafe extern "C" fn counting_rejecting_dispatch(
        dispatcher: *mut c_void,
        task: MigoTaskFn,
        context: *mut c_void,
    ) -> MigoResult {
        DISPATCH_CALLS.fetch_add(1, Ordering::SeqCst);
        unsafe { rejecting_dispatch(dispatcher, task, context) }
    }

    unsafe extern "C" fn queued_dispatch(
        _dispatcher: *mut c_void,
        task: MigoTaskFn,
        context: *mut c_void,
    ) -> MigoResult {
        *QUEUED_TASK
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some((task, context as usize));
        MIGO_OK
    }

    struct BlockingDispatch {
        state: Mutex<(bool, bool)>,
        changed: Condvar,
    }

    unsafe extern "C" fn blocking_dispatch(
        dispatcher: *mut c_void,
        _task: MigoTaskFn,
        _context: *mut c_void,
    ) -> MigoResult {
        let dispatcher = unsafe { &*(dispatcher as *const BlockingDispatch) };
        let mut state = dispatcher
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.0 = true;
        dispatcher.changed.notify_all();
        while !state.1 {
            state = dispatcher
                .changed
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
        MIGO_ERROR_INVALID_ARGUMENT
    }

    unsafe extern "C" fn on_ready(_user: *mut c_void, _session: *mut c_void) {
        READY_CALLS.fetch_add(1, Ordering::SeqCst);
    }

    unsafe extern "C" fn on_ready_destroy(_user: *mut c_void, session: *mut c_void) {
        let result = unsafe { migo_session_destroy(session.cast::<MigoSession>()) };
        DESTROY_RESULT.store(result, Ordering::SeqCst);
    }

    unsafe extern "C" fn on_error(
        _user: *mut c_void,
        _session: *mut c_void,
        error: *const MigoError,
    ) {
        let error = unsafe { &*error };
        LAST_ERROR_CODE.store((-error.code) as usize, Ordering::SeqCst);
        // The message must be readable for the duration of this callback.
        assert!(!error.message_utf8.is_null());
        let text = unsafe { std::ffi::CStr::from_ptr(error.message_utf8) };
        assert!(!text.to_bytes().is_empty());
    }

    fn callbacks(dispatch: MigoDispatchFn) -> HostCallbacks {
        HostCallbacks {
            user_data: std::ptr::null_mut(),
            dispatcher_data: std::ptr::null_mut(),
            dispatch,
            on_ready: Some(on_ready),
            on_error: Some(on_error),
            on_exit_requested: None,
            on_surface_lost: None,
            on_request_frame: None,
            on_show_keyboard: None,
            on_hide_keyboard: None,
            on_update_keyboard: None,
            on_surface_released: None,
            on_vibrate: None,
            on_keep_screen_on: None,
            on_game_log: None,
        }
    }

    static SHOWN_MAX_LENGTH: AtomicUsize = AtomicUsize::new(0);
    static SHOWN_DEFAULT_VALUE: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

    unsafe extern "C" fn noop_show_keyboard(
        _user: *mut c_void,
        _session: *mut c_void,
        _options: *const MigoKeyboardShowOptions,
    ) {
    }
    unsafe extern "C" fn noop_hide_keyboard(_user: *mut c_void, _session: *mut c_void) {}
    unsafe extern "C" fn noop_update_keyboard(
        _user: *mut c_void,
        _session: *mut c_void,
        _value_utf8: *const c_char,
        _value_length: u32,
    ) {
    }

    unsafe extern "C" fn recording_show_keyboard(
        _user: *mut c_void,
        _session: *mut c_void,
        options: *const MigoKeyboardShowOptions,
    ) {
        let options = unsafe { &*options };
        SHOWN_MAX_LENGTH.store(options.max_length as usize, Ordering::SeqCst);
        let bytes = unsafe {
            std::slice::from_raw_parts(
                options.default_value_utf8 as *const u8,
                options.default_value_length as usize,
            )
        };
        *SHOWN_DEFAULT_VALUE
            .lock()
            .unwrap_or_else(|e| e.into_inner()) =
            std::str::from_utf8(bytes).expect("utf-8").to_string();
    }

    /// A raw callback set with a working dispatcher and no keyboard, which the
    /// install tests then vary one field at a time.
    fn raw_callbacks() -> MigoHostCallbacks {
        MigoHostCallbacks {
            header: VersionedHeader {
                struct_size: size_of::<MigoHostCallbacks>() as u32,
                abi_version: migo_capi_abi::MIGO_ABI_VERSION_CURRENT,
            },
            user_data: std::ptr::null_mut(),
            dispatcher_data: std::ptr::null_mut(),
            dispatch: Some(inline_dispatch),
            on_ready: None,
            on_error: None,
            on_exit_requested: None,
            on_surface_lost: None,
            on_request_frame: None,
            on_show_keyboard: None,
            on_hide_keyboard: None,
            on_update_keyboard: None,
            on_surface_released: None,
            on_vibrate: None,
            on_keep_screen_on: None,
            on_game_log: None,
        }
    }

    fn keyboard_notifier(
        dispatch: MigoDispatchFn,
        on_show_keyboard: Option<MigoOnShowKeyboardFn>,
    ) -> TestNotifier {
        let callbacks = HostCallbacks {
            user_data: std::ptr::null_mut(),
            dispatcher_data: std::ptr::null_mut(),
            dispatch,
            on_ready: None,
            on_error: None,
            on_exit_requested: None,
            on_surface_lost: None,
            on_request_frame: None,
            on_show_keyboard,
            on_hide_keyboard: Some(noop_hide_keyboard),
            on_update_keyboard: Some(noop_update_keyboard),
            on_surface_released: None,
            on_vibrate: None,
            on_keep_screen_on: None,
            on_game_log: None,
        };
        test_notifier(callbacks)
    }

    /// Storage aligned like the real struct, so a short caller can be built
    /// byte-exactly. A `Vec<u8>` would not be guaranteed 8-aligned, and reading
    /// a `struct_size` out of a misaligned buffer is not the thing under test.
    #[repr(align(8))]
    struct ShortCaller([u8; size_of::<MigoHostCallbacks>()]);

    /// Build the first `size` bytes of a full callback set, as an older client's
    /// compiled code would have written them.
    fn truncated_caller(size: usize) -> ShortCaller {
        let mut full = raw_callbacks();
        full.header.struct_size = size as u32;
        let mut buffer = ShortCaller([0u8; size_of::<MigoHostCallbacks>()]);
        // SAFETY: both are exactly `size_of::<MigoHostCallbacks>()` bytes, and
        // the struct is plain data.
        unsafe {
            std::ptr::copy_nonoverlapping(
                &full as *const MigoHostCallbacks as *const u8,
                buffer.0.as_mut_ptr(),
                size_of::<MigoHostCallbacks>(),
            );
        }
        // Everything past the announced size belongs to nobody: an older client
        // never allocated it, so leaving real bytes there would let a passing
        // test hide a read past the caller's storage.
        for byte in &mut buffer.0[size..] {
            *byte = 0xAA;
        }
        buffer
    }

    /// A host compiled before the keyboard existed must still install.
    ///
    /// This is the case `session.h` has promised since the frame-pacing slice
    /// and the code did not honour: exact-size validation rejected it outright.
    #[test]
    fn a_client_from_before_the_keyboard_still_installs() {
        // 72 bytes: through `on_request_frame`, which is what the struct was
        // before the keyboard verbs were appended.
        let buffer = truncated_caller(72);
        let copied = unsafe {
            migo_capi_abi::copy_versioned::<MigoHostCallbacks>(
                buffer.0.as_ptr() as *const VersionedHeader
            )
        }
        .expect("a client from the previous header must be accepted");

        assert!(
            copied.on_show_keyboard.is_none()
                && copied.on_hide_keyboard.is_none()
                && copied.on_update_keyboard.is_none(),
            "fields that client never had must read as absent, not as its neighbouring bytes"
        );
        assert!(
            !copied
                .validate()
                .expect("valid")
                .expect("dispatcher is configured")
                .supplies_keyboard()
        );
    }

    /// The documented floor: a dispatcher and nothing else. A host that wants no
    /// notifications at all is quiet, not wrong.
    #[test]
    fn a_client_with_only_a_dispatcher_installs() {
        let buffer = truncated_caller(32);
        let copied = unsafe {
            migo_capi_abi::copy_versioned::<MigoHostCallbacks>(
                buffer.0.as_ptr() as *const VersionedHeader
            )
        }
        .expect("the minimum must be accepted");
        assert!(copied.dispatch.is_some());
        assert!(copied.on_ready.is_none());
    }

    /// One byte below the floor is missing part of `dispatch`, and there is no
    /// call to make without it.
    #[test]
    fn a_client_below_the_minimum_is_rejected() {
        let buffer = truncated_caller(31);
        assert_eq!(
            unsafe {
                migo_capi_abi::copy_versioned::<MigoHostCallbacks>(
                    buffer.0.as_ptr() as *const VersionedHeader
                )
            }
            .err(),
            Some(MIGO_ERROR_INVALID_ARGUMENT)
        );
    }

    /// A bigger struct is a newer contract, and the caller needs to hear that
    /// rather than have its unknown fields silently dropped.
    #[test]
    fn a_client_from_a_newer_header_is_told_the_abi_is_unsupported() {
        let mut raw = raw_callbacks();
        raw.header.struct_size = size_of::<MigoHostCallbacks>() as u32 + 8;
        assert_eq!(
            unsafe {
                migo_capi_abi::copy_versioned::<MigoHostCallbacks>(
                    &raw as *const MigoHostCallbacks as *const VersionedHeader,
                )
            }
            .err(),
            Some(migo_capi_abi::MIGO_ERROR_UNSUPPORTED_ABI)
        );
    }

    /// A host that can open a keyboard but not close it strands it on screen
    /// with no event that corrects the state. Partial support is refused at
    /// install time, where the host can still fix it.
    #[test]
    fn keyboard_callbacks_install_all_or_none() {
        // None of the three: accepted, and the capability is not offered.
        let copied = raw_callbacks()
            .validate()
            .expect("none is valid")
            .expect("dispatcher is configured");
        assert!(!copied.supplies_keyboard());

        // Each one alone, then two of three: all partial, all refused.
        let mut show_only = raw_callbacks();
        show_only.on_show_keyboard = Some(noop_show_keyboard);
        let mut hide_only = raw_callbacks();
        hide_only.on_hide_keyboard = Some(noop_hide_keyboard);
        let mut update_only = raw_callbacks();
        update_only.on_update_keyboard = Some(noop_update_keyboard);
        let mut two = raw_callbacks();
        two.on_show_keyboard = Some(noop_show_keyboard);
        two.on_hide_keyboard = Some(noop_hide_keyboard);

        for (name, partial) in [
            ("show only", &show_only),
            ("hide only", &hide_only),
            ("update only", &update_only),
            ("two of three", &two),
        ] {
            assert_eq!(
                partial.validate().err(),
                Some(MIGO_ERROR_INVALID_ARGUMENT),
                "{name} must be refused"
            );
        }

        // All three: accepted, and the capability is offered.
        let mut all = raw_callbacks();
        all.on_show_keyboard = Some(noop_show_keyboard);
        all.on_hide_keyboard = Some(noop_hide_keyboard);
        all.on_update_keyboard = Some(noop_update_keyboard);
        let copied = all
            .validate()
            .expect("all three is valid")
            .expect("dispatcher is configured");
        assert!(copied.supplies_keyboard());
    }

    /// The verbs must report whether the dispatcher took the task. Reporting
    /// success for a refused dispatch would tell content its keyboard is
    /// opening when nothing will ever open it.
    #[test]
    fn a_refused_dispatch_is_reported_to_the_caller() {
        let accepting = keyboard_notifier(inline_dispatch, Some(noop_show_keyboard));
        assert!(accepting.show_keyboard(ShowOptions::default()));
        assert!(accepting.hide_keyboard());
        assert!(accepting.update_keyboard("x".to_string()));

        let refusing = keyboard_notifier(rejecting_dispatch, Some(noop_show_keyboard));
        assert!(!refusing.show_keyboard(ShowOptions::default()));
        assert!(!refusing.hide_keyboard());
        assert!(!refusing.update_keyboard("x".to_string()));
    }

    /// The options and their string are borrowed for the callback only, so the
    /// payload must still own them while the host reads them. A payload dropped
    /// before the call would hand the host a pointer into freed memory.
    #[test]
    fn show_options_reach_the_host_intact() {
        let _guard = COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        SHOWN_MAX_LENGTH.store(0, Ordering::SeqCst);
        SHOWN_DEFAULT_VALUE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();

        let notifier = keyboard_notifier(inline_dispatch, Some(recording_show_keyboard));
        assert!(notifier.show_keyboard(ShowOptions {
            flags: MIGO_KEYBOARD_FLAG_MULTIPLE,
            max_length: 140,
            confirm_type: MIGO_KEYBOARD_CONFIRM_SEND,
            keyboard_type: MIGO_KEYBOARD_TYPE_TEXT,
            default_value: "seed".to_string(),
        }));

        assert_eq!(SHOWN_MAX_LENGTH.load(Ordering::SeqCst), 140);
        assert_eq!(
            *SHOWN_DEFAULT_VALUE
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
            "seed"
        );
    }

    fn notifier(dispatch: MigoDispatchFn) -> TestNotifier {
        test_notifier(callbacks(dispatch))
    }

    #[test]
    fn events_reach_the_host_through_its_dispatcher() {
        // The engine must never call host code directly: the dispatcher is what
        // moves it to a thread the host chose.
        let _guard = COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        READY_CALLS.store(0, Ordering::SeqCst);
        DISPATCH_CALLS.store(0, Ordering::SeqCst);
        let notifier = notifier(counting_inline_dispatch);
        notifier.ready();
        assert_eq!(DISPATCH_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(READY_CALLS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_destroyed_session_cancels_queued_callbacks() {
        // Header rule: destroy cancels queued callbacks. Without this the host
        // would be handed a session pointer it has already released.
        let _guard = COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        READY_CALLS.store(0, Ordering::SeqCst);
        let notifier = notifier(inline_dispatch);
        notifier
            .session
            .callback_gate
            .close()
            .expect("first close")
            .wait();
        notifier.ready();
        assert_eq!(
            READY_CALLS.load(Ordering::SeqCst),
            0,
            "no callback may run after the session is gone"
        );
    }

    #[test]
    fn a_task_accepted_before_close_is_canceled_when_it_starts_after_close() {
        let _guard = COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        READY_CALLS.store(0, Ordering::SeqCst);
        *QUEUED_TASK
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        let notifier = notifier(queued_dispatch);

        notifier.ready();
        notifier
            .session
            .callback_gate
            .close()
            .expect("first close")
            .wait();
        let (task, context) = QUEUED_TASK
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .expect("dispatcher accepted one task");
        unsafe { task(context as *mut c_void) };

        assert_eq!(READY_CALLS.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn close_waits_for_a_dispatch_call_already_inside_host_code() {
        let dispatcher = BlockingDispatch {
            state: Mutex::new((false, false)),
            changed: Condvar::new(),
        };
        let mut installed = callbacks(blocking_dispatch);
        installed.dispatcher_data = (&dispatcher as *const BlockingDispatch).cast_mut().cast();
        let notifier = test_notifier(installed);
        let (drained_tx, drained_rx) = mpsc::channel();

        std::thread::scope(|scope| {
            scope.spawn(|| notifier.ready());

            let mut state = dispatcher
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            while !state.0 {
                state = dispatcher
                    .changed
                    .wait(state)
                    .unwrap_or_else(|error| error.into_inner());
            }
            drop(state);

            let drain = notifier.session.callback_gate.close().expect("first close");
            scope.spawn(move || {
                drain.wait();
                drained_tx.send(()).expect("signal drain");
            });
            assert!(
                drained_rx.recv_timeout(Duration::from_millis(25)).is_err(),
                "destroy must not pass a dispatcher still using dispatcher_data",
            );

            let mut state = dispatcher
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.1 = true;
            dispatcher.changed.notify_all();
            drop(state);
            drained_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("drain completes after dispatch returns");
        });
    }

    #[test]
    fn a_callback_can_destroy_its_session_and_the_task_pins_allocation_until_return() {
        let _guard = COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        with_engine("callback-reentrant-destroy", |engine| {
            let mut raw_session = std::ptr::null_mut();
            assert_eq!(
                unsafe { migo_session_create(engine, &session_config(), &mut raw_session) },
                MIGO_OK,
            );
            let session = unsafe { crate::pin_session(raw_session) }.expect("Session pin");
            let mut installed = callbacks(inline_dispatch);
            installed.on_ready = Some(on_ready_destroy);
            let notifier = Notifier::new(installed, Arc::downgrade(&session));
            DESTROY_RESULT.store(i32::MIN, Ordering::SeqCst);

            notifier.ready();

            assert_eq!(DESTROY_RESULT.load(Ordering::SeqCst), MIGO_OK);
            assert!(!session.is_open());
            assert_eq!(Arc::strong_count(&session), 1);
        });
    }

    #[test]
    fn a_rejected_dispatch_drops_the_task_instead_of_leaking_or_running() {
        let _guard = COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        READY_CALLS.store(0, Ordering::SeqCst);
        DISPATCH_CALLS.store(0, Ordering::SeqCst);
        let notifier = notifier(counting_rejecting_dispatch);
        notifier.ready();
        assert_eq!(DISPATCH_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(READY_CALLS.load(Ordering::SeqCst), 0);
        // Miri/leak checkers would catch a leak here; the point of the test is
        // that ownership returned to us on rejection.
    }

    #[test]
    fn error_messages_are_delivered_as_readable_c_strings() {
        let _guard = COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        LAST_ERROR_CODE.store(0, Ordering::SeqCst);
        let notifier = notifier(inline_dispatch);
        notifier.error(MIGO_ERROR_INVALID_ARGUMENT, "content failed to load");
        assert_eq!(LAST_ERROR_CODE.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn interior_nul_messages_still_report_the_error() {
        let _guard = COUNTER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Losing the report entirely would be worse than losing the detail.
        LAST_ERROR_CODE.store(0, Ordering::SeqCst);
        let notifier = notifier(inline_dispatch);
        notifier.error(MIGO_ERROR_INVALID_ARGUMENT, "bad\0message");
        assert_eq!(LAST_ERROR_CODE.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn callbacks_without_a_dispatcher_are_refused() {
        let mut raw = raw_callbacks();
        raw.dispatch = None;
        raw.on_ready = Some(on_ready);
        assert_eq!(raw.validate().err(), Some(MIGO_ERROR_INVALID_ARGUMENT));
    }
}
