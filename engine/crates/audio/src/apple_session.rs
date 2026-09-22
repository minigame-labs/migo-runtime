//! The engine's `AVAudioSession`, on iOS.
//!
//! An iOS process plays through one session, and until it has a category and is
//! active, the output unit this crate opens is silent or refuses to start.
//! There is no host call for that on this lane -- the C ABI has no audio
//! entry point, and the Performance+ host is a Swift app that never sees this
//! crate -- so the engine configures its own session, which is also the only
//! thing `setInnerAudioOption` can act on:
//!
//! - `obeyMuteSwitch` chooses the category: `Ambient` respects the silent
//!   switch, `Playback` plays through it.
//! - `mixWithOther` adds `MixWithOthers`, so another app's music keeps playing.
//! - `speakerOn` is the speaker, which both categories already route to. The
//!   receiver route needs `PlayAndRecord`, and that asks for the microphone --
//!   a permission this profile does not grant -- so a `false` is accepted and
//!   not honoured, as Android's own best-effort `setSpeakerphoneOn` is.
//!
//! The session also says when the system took audio away: a phone call, Siri,
//! another app. That arrives as `AVAudioSessionInterruptionNotification`, and
//! this delivers it to the host as `OnAudioInterruptionBegin`/`End`, which both
//! executions hand to content's `onAudioInterruptionBegin` listeners. The
//! output unit is stopped by the system at a begin and has to be started again
//! after an end, which the audio thread does through [`InterruptionState`].
//!
//! Written against the Objective-C runtime directly, as
//! `migo-platform`'s Apple presenter is: two `objc_msgSend` casts and a class
//! built at run time for the notification, against one framework the archive
//! already links -- rather than a binding crate for four calls.

#![cfg(target_os = "ios")]

use std::ffi::{CString, c_char, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use shared::error::{EngineError, EngineResult, ErrorCode};
use shared::protocol::host_cmd::HostCommand;
use tracing::{info, warn};

type Object = c_void;
type Sel = *const c_void;
type Class = *const c_void;
type Imp = unsafe extern "C" fn();

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> Class;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_msgSend();
    fn objc_allocateClassPair(superclass: Class, name: *const c_char, extra: usize) -> Class;
    fn objc_registerClassPair(class: Class);
    fn class_addMethod(class: Class, name: Sel, imp: Imp, types: *const c_char) -> bool;
}

#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {
    static AVAudioSessionCategoryAmbient: *const Object;
    static AVAudioSessionCategoryPlayback: *const Object;
    static AVAudioSessionInterruptionNotification: *const Object;
    static AVAudioSessionInterruptionTypeKey: *const Object;
}

/// `AVAudioSessionCategoryOptionMixWithOthers`.
const MIX_WITH_OTHERS: u64 = 1;
/// `AVAudioSessionInterruptionTypeBegan`.
const INTERRUPTION_BEGAN: u64 = 1;

fn selector(name: &str) -> Sel {
    let name = CString::new(name).expect("a selector has no interior nul");
    unsafe { sel_registerName(name.as_ptr()) }
}

fn class(name: &str) -> Option<Class> {
    let name = CString::new(name).ok()?;
    let class = unsafe { objc_getClass(name.as_ptr()) };
    (!class.is_null()).then_some(class)
}

/// `[receiver selector]`.
unsafe fn send(receiver: *const Object, sel: Sel) -> *mut Object {
    let msg: unsafe extern "C" fn(*const Object, Sel) -> *mut Object =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { msg(receiver, sel) }
}

/// What content asked of the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioSessionOptions {
    pub mix_with_other: bool,
    pub obey_mute_switch: bool,
    pub speaker_on: bool,
}

impl Default for AudioSessionOptions {
    /// The mini-game API's defaults: the silent switch is obeyed and other
    /// apps' audio keeps playing.
    fn default() -> Self {
        Self {
            mix_with_other: true,
            obey_mute_switch: true,
            speaker_on: true,
        }
    }
}

struct SessionState {
    options: AudioSessionOptions,
    applied: bool,
}

fn state() -> &'static Mutex<SessionState> {
    static STATE: OnceLock<Mutex<SessionState>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(SessionState {
            options: AudioSessionOptions::default(),
            applied: false,
        })
    })
}

/// Whether the system has taken audio away, and who to tell when it does.
///
/// Process-wide because the session is: every live session's host channel gets
/// the interruption, as every one of them is losing its output.
pub struct InterruptionState {
    interrupted: AtomicBool,
    listeners: Mutex<Vec<shared::op_state::HostTx>>,
}

impl InterruptionState {
    /// True while the system holds the audio: the output unit is stopped and
    /// starting it again would fail until an end arrives.
    pub fn is_interrupted(&self) -> bool {
        self.interrupted.load(Ordering::Acquire)
    }
}

fn interruptions() -> &'static InterruptionState {
    static STATE: OnceLock<InterruptionState> = OnceLock::new();
    STATE.get_or_init(|| InterruptionState {
        interrupted: AtomicBool::new(false),
        listeners: Mutex::new(Vec::new()),
    })
}

/// The process's interruption state, for the audio thread's stream gate.
pub fn interruption_state() -> &'static InterruptionState {
    interruptions()
}

/// Tell this session's host about interruptions from now on.
///
/// Idempotent per channel in effect: a session registers once, when its audio
/// thread starts, and its sender is dropped with the session.
pub fn watch_interruptions(host_tx: shared::op_state::HostTx) {
    interruptions()
        .listeners
        .lock()
        .expect("the interruption listeners are never poisoned")
        .push(host_tx);
    install_observer();
}

fn deliver(interrupted: bool) {
    interruptions()
        .interrupted
        .store(interrupted, Ordering::Release);
    // Built per listener: a host command owns what it carries and is not
    // cloneable, and these two carry nothing.
    let command = || {
        if interrupted {
            HostCommand::OnAudioInterruptionBegin
        } else {
            HostCommand::OnAudioInterruptionEnd
        }
    };
    let mut listeners = interruptions()
        .listeners
        .lock()
        .expect("the interruption listeners are never poisoned");
    // A host whose session has gone takes nothing more; dropping it here is
    // how this list stays the live ones.
    listeners.retain(|host_tx| host_tx.try_send(command()).is_ok());
}

/// The notification's `AVAudioSessionInterruptionTypeKey`, as a began/ended.
unsafe extern "C" fn on_interruption(_this: *const Object, _sel: Sel, notification: *const Object) {
    let user_info = unsafe { send(notification, selector("userInfo")) };
    if user_info.is_null() {
        return;
    }
    let object_for_key: unsafe extern "C" fn(*const Object, Sel, *const Object) -> *mut Object =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    let value = unsafe {
        object_for_key(
            user_info,
            selector("objectForKey:"),
            AVAudioSessionInterruptionTypeKey,
        )
    };
    if value.is_null() {
        return;
    }
    let unsigned_value: unsafe extern "C" fn(*const Object, Sel) -> u64 =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    let kind = unsafe { unsigned_value(value, selector("unsignedIntegerValue")) };
    deliver(kind == INTERRUPTION_BEGAN);
}

/// Register one observer object for the process, built at run time because the
/// notification centre calls a selector on an object and this crate has no
/// Objective-C class of its own.
fn install_observer() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let Some(superclass) = class("NSObject") else {
            warn!("no NSObject: audio interruptions will not reach content");
            return;
        };
        let name = CString::new("MigoAudioSessionObserver").expect("a class name");
        let observer_class = unsafe { objc_allocateClassPair(superclass, name.as_ptr(), 0) };
        if observer_class.is_null() {
            warn!("could not build the audio interruption observer class");
            return;
        }
        // `v@:@`: returns void, takes self, _cmd and the notification.
        let types = CString::new("v@:@").expect("a type encoding");
        let added = unsafe {
            class_addMethod(
                observer_class,
                selector("onInterruption:"),
                std::mem::transmute::<unsafe extern "C" fn(*const Object, Sel, *const Object), Imp>(
                    on_interruption,
                ),
                types.as_ptr(),
            )
        };
        unsafe { objc_registerClassPair(observer_class) };
        if !added {
            warn!("could not add the audio interruption handler");
            return;
        }
        let observer = unsafe { send(send(observer_class, selector("alloc")), selector("init")) };
        let Some(center_class) = class("NSNotificationCenter") else {
            return;
        };
        let center = unsafe { send(center_class, selector("defaultCenter")) };
        let add_observer: unsafe extern "C" fn(
            *const Object,
            Sel,
            *const Object,
            Sel,
            *const Object,
            *const Object,
        ) = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
        unsafe {
            add_observer(
                center,
                selector("addObserver:selector:name:object:"),
                observer,
                selector("onInterruption:"),
                AVAudioSessionInterruptionNotification,
                ptr::null(),
            );
        }
        // The observer is deliberately never released: it lives as long as the
        // process's session does, and an observer freed while registered is a
        // message to a dead object.
        info!("audio interruptions are observed");
    });
}

/// An `NSError`'s code, for a refusal a person has to act on: the four-letter
/// OSStatus AVFoundation reports, as a number.
fn error_code(error: *mut Object) -> String {
    if error.is_null() {
        return "no error reported".to_string();
    }
    let code: unsafe extern "C" fn(*const Object, Sel) -> isize =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    format!("NSError code {}", unsafe { code(error, selector("code")) })
}

fn shared_session() -> Option<*mut Object> {
    let session_class = class("AVAudioSession")?;
    let session = unsafe { send(session_class, selector("sharedInstance")) };
    (!session.is_null()).then_some(session)
}

/// Apply `options` to the process's session and activate it.
///
/// Called before the output opens and again whenever content sets the option,
/// because a category set while the unit is running takes effect on the next
/// route change and both have to end at the same state.
pub fn configure(options: AudioSessionOptions) -> EngineResult<()> {
    let session = shared_session().ok_or_else(|| {
        EngineError::from_detail(
            ErrorCode::Unsupported,
            "setInnerAudioOption:fail this process has no AVAudioSession",
        )
    })?;
    let category = unsafe {
        if options.obey_mute_switch {
            AVAudioSessionCategoryAmbient
        } else {
            AVAudioSessionCategoryPlayback
        }
    };
    let category_options = if options.mix_with_other {
        MIX_WITH_OTHERS
    } else {
        0
    };
    let set_category: unsafe extern "C" fn(
        *const Object,
        Sel,
        *const Object,
        u64,
        *mut *mut Object,
    ) -> bool = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    let mut error: *mut Object = ptr::null_mut();
    let set = unsafe {
        set_category(
            session,
            selector("setCategory:withOptions:error:"),
            category,
            category_options,
            &mut error,
        )
    };
    if !set {
        return Err(EngineError::from_detail(
            ErrorCode::Internal,
            format!(
                "setInnerAudioOption:fail the audio session refused the category ({})",
                error_code(error)
            ),
        ));
    }
    let set_active: unsafe extern "C" fn(*const Object, Sel, bool, *mut *mut Object) -> bool =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    let mut error: *mut Object = ptr::null_mut();
    let active = unsafe { set_active(session, selector("setActive:error:"), true, &mut error) };
    if !active {
        return Err(EngineError::from_detail(
            ErrorCode::Internal,
            format!(
                "setInnerAudioOption:fail the audio session could not be activated ({})",
                error_code(error)
            ),
        ));
    }
    info!(
        "audio session active: category={}, mixWithOthers={}",
        if options.obey_mute_switch {
            "ambient"
        } else {
            "playback"
        },
        options.mix_with_other
    );
    let mut state = state().lock().expect("the session state is never poisoned");
    state.options = options;
    state.applied = true;
    Ok(())
}

/// Make sure the session is configured before the output unit opens: with what
/// content asked for, or with the defaults if it has not asked.
pub fn ensure_configured() {
    let options = {
        let state = state().lock().expect("the session state is never poisoned");
        if state.applied {
            return;
        }
        state.options
    };
    if let Err(error) = configure(options) {
        // Not fatal: the unit may still open under the process's own session,
        // and a game without sound is better than a session that will not start.
        warn!("the audio session was not configured: {error}");
    }
}

/// `setInnerAudioOption`, as the platform audio service.
pub struct AppleAudioPlatform;

impl shared::services::AudioPlatformService for AppleAudioPlatform {
    fn set_inner_audio_option(
        &self,
        mix_with_other: bool,
        obey_mute_switch: bool,
        speaker_on: bool,
    ) -> Result<(), shared::protocol::error::ServiceError> {
        configure(AudioSessionOptions {
            mix_with_other,
            obey_mute_switch,
            speaker_on,
        })
        .map_err(|error| {
            shared::protocol::error::ServiceError::system(
                error.detail.unwrap_or_else(|| error.msg.to_string()),
            )
        })
    }
}

/// The platform's audio service: the process's session, which is the engine's
/// on this platform.
pub fn platform_service() -> Arc<dyn shared::services::AudioPlatformService> {
    static SERVICE: OnceLock<Arc<AppleAudioPlatform>> = OnceLock::new();
    SERVICE.get_or_init(|| Arc::new(AppleAudioPlatform)).clone()
}
