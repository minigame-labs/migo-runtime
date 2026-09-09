//! The `migo_*` C ABI implementation.
//!
//! Implements the entry points declared in `include/migo/`. The headers are the
//! specification; this crate is the only place that turns them into engine
//! calls, and every rule they state (versioned structs, borrowed-for-the-call
//! strings, handles consumed on success) is enforced by `migo-capi-abi`,
//! which is dependency-free so those rules stay provable without a device.
//! The one rule that cannot live there is the panic barrier, because it
//! wraps engine calls: see [`panic_barrier`].
//!
//! The implementation is embedding-first: hosts retain their native window and
//! event-loop ownership, while Migo owns engine/session state and explicit
//! asynchronous Surface retirement. Platform availability and ABI stability
//! are advertised by the public headers and artifact manifests, not inferred
//! from which Rust modules happened to compile.

// Section 7.3's steady-state growth gate reads this. `#[cfg(test)]` scopes it to this
// crate's own test binary: a `#[global_allocator]` is unique per binary, so one declared
// unconditionally here would follow this crate into the cdylib it exists to produce.
// Deleting it does not make the gate pass silently -- every cycle proves the allocator
// observes both ends before it trusts a delta.
#[cfg(test)]
#[global_allocator]
static COUNTING_ALLOCATOR: migo_alloc_probe::CountingAllocator =
    migo_alloc_probe::CountingAllocator::system();

#[cfg(target_os = "android")]
mod android;
mod callback_gate;
mod callbacks;
mod capabilities;
#[cfg(test)]
mod concurrent_sessions;
mod gamepad;
mod host_kit;
mod input;
mod keyboard;
mod layout;
mod panic_barrier;
mod platform;
// Which execution this product compiled: a JavaScript runtime in this process,
// or frames produced by one somewhere else.
mod retirement;
mod session_engine;
// The frame transport's entry points, in the product that has a transport.
#[cfg(feature = "external-frames")]
mod external_frames;
mod surface;
#[cfg(test)]
mod test_support;

// The surface entry points and the descriptors they read live in their
// own module; re-exported so the crate's public surface is unchanged.
pub use gamepad::{
    MigoGamepadButton, MigoGamepadInfo, MigoGamepadStateEvent, migo_session_send_gamepad_state,
    migo_session_set_gamepad_connected,
};
pub use input::{
    MigoPointerEvent, MigoTouchEvent, MigoTouchPoint, MigoWheelEvent,
    migo_session_send_pointer_event, migo_session_send_touch, migo_session_send_wheel_event,
};
pub use keyboard::{
    MigoCompositionEvent, MigoKeyEvent, MigoKeyboardEvent, migo_session_send_composition_event,
    migo_session_send_key_event, migo_session_send_keyboard_event,
};
pub use migo_capi_abi::config::{MigoContentDescriptor, MigoEngineConfig, MigoSessionConfig};
pub use surface::{
    MigoAndroidNativeWindowDescriptor, MigoSurfaceAttachment, MigoSurfaceDescriptor,
    MigoSurfaceMetrics, MigoSurfaceRelease, MigoSurfaceReleaseStatus, MigoWaylandSurfaceDescriptor,
    MigoX11WindowDescriptor, migo_session_attach_surface, migo_surface_begin_detach,
    migo_surface_release_destroy, migo_surface_release_query, migo_surface_update,
};

#[cfg(test)]
use migo_capi_abi::config::MIGO_ENGINE_FLAG_ALLOW_UNSIGNED_CONTENT;

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

use graphics::egl_platform::PlatformIdentity;
use migo_capi_abi::{
    MIGO_ERROR_INTERNAL, MIGO_ERROR_INVALID_ARGUMENT, MIGO_ERROR_INVALID_STATE,
    MIGO_ERROR_WOULD_BLOCK, MIGO_OK, MigoResult,
};
use migo_core::send_command_to_host;

use crate::session_engine::SessionEngine;
use panic_barrier::guard;
use shared::surface::HostWindowState;
use shared::{config::InitOptions, protocol::host_cmd::HostCommand, surface::SurfaceLossReason};

// ---- Handles ----------------------------------------------------------------

/// Process-level state: the storage roots the host granted, plus a live-session
/// count so `migo_engine_destroy` can enforce the header's rule that children
/// go first instead of leaving sessions pointing at freed configuration.
///
/// # Why the three roots are the Engine's and not each Session's
///
/// Two games in one Engine are separated *below* these roots, at the game
/// identity: every path a Session touches is `<root>/migo/games/<content_id>/…`.
/// Moving the roots to Session scope would not add that separation — a host could
/// still hand two Sessions one root and one content id — while each root has its
/// own reason to stay where it is.
///
/// `files_dir` and `cache_dir` are the *host application's* directories, granted
/// to it once by the platform: one `Context.getFilesDir()` per Android app, one
/// `NSDocumentDirectory` per iOS app. There is no second one for the engine to
/// take. A host that genuinely needs two games on two different volumes creates a
/// second Engine, which the ABI allows and `concurrent_sessions` covers.
///
/// `code_cache_dir` is shared **on purpose**, and this is the root that would be
/// actively damaged by moving: Section 6.5 requires the on-disk V8 code cache to
/// be shared between Sessions, its key is `hash(source_bytes, v8_version)` so the
/// bytes mean the same thing to whoever asked, and the budget task 0.19 moved onto
/// the directory is a budget *because* the directory is one. Per-Session roots
/// would silently give each Session its own copy of every compile.
struct EngineInner {
    files_dir: PathBuf,
    cache_dir: PathBuf,
    code_cache_dir: PathBuf,
    /// `MIGO_ENGINE_FLAG_ALLOW_UNSIGNED_CONTENT`: opt-in, never a default.
    allow_unsigned_content: bool,
    live_sessions: Mutex<usize>,
    retired_hosts: retirement::RetirementSet,
}

impl EngineInner {
    /// The options every Session of this Engine starts a Host from.
    ///
    /// One place, deliberately. The storage roots are the whole of what
    /// `MigoEngineConfig` grants, and a per-Session override added at one
    /// construction site and missed at another is how "every Session of an Engine
    /// shares these roots" stops being true without anything failing. There is
    /// nothing to keep in step because there is nothing to keep in step *with*.
    ///
    /// The roots pass through verbatim: the header states that Migo never invents
    /// a location of its own, so deriving a subdirectory here would put game data
    /// somewhere the host cannot find, clear, or back up.
    fn session_init_options(&self, pixel_ratio: f32) -> InitOptions {
        let options = InitOptions::new()
            .with_files_dir(self.files_dir.clone())
            .with_cache_dir(self.cache_dir.clone())
            .with_code_cache_dir(self.code_cache_dir.clone())
            .with_pixel_ratio(pixel_ratio)
            .with_code_signing_enabled(!self.allow_unsigned_content);
        // The session's level, not just the process default, because a session
        // binds its own level to its host thread and publishes it for the render
        // thread -- deliberately, so one session cannot silence another. Setting
        // only the process default therefore left `MIGO_CAPI_LOG` unable to raise
        // the level for any thread that does engine work: measured on Android as
        // two Warn records and no Info at all, from a host that had asked for
        // Info. This is the whole of the diagnostic channel a host with no Java
        // has, so it has to reach the threads whose behaviour is in question.
        match dev_log_level() {
            Some(level) => options.with_log_level(level),
            None => options,
        }
    }
}

pub struct MigoEngine {
    inner: Arc<EngineInner>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum SurfaceTransition {
    #[default]
    Idle,
    Attaching,
    Updating,
    Detaching,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum LifecycleState {
    #[default]
    Created,
    Running,
    Paused,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingSurfaceLoss {
    generation: u64,
    reason: SurfaceLossReason,
}

const fn platform_state_is_consistent(
    host_present: bool,
    identity_present: bool,
    context_present: bool,
) -> bool {
    host_present == identity_present && identity_present == context_present
}

struct SessionState {
    lifecycle: LifecycleState,
    /// Installed once, before the first attach, per the header's rule that a
    /// queued task must never see a replaced pointer.
    callbacks: Option<callbacks::HostCallbacks>,
    /// Distinguishes a deliberately empty callback set from not configured.
    callbacks_configured: bool,
    /// Frozen at the first attach or RUNNING transition.
    callbacks_frozen: bool,
    /// Set once a surface has been attached; the engine host thread owns the
    /// render loop from that point.
    host: Option<SessionEngine>,
    /// Immutable graphics domain committed atomically with `host`.
    platform_identity: Option<PlatformIdentity>,
    /// Target-specific construction owner committed atomically with `host`.
    ///
    /// Reattachment clones this state to validate and reuse the exact EGL
    /// domain. In particular, Linux X11 stores Migo's private connection here
    /// rather than rebuilding from a caller-owned `Display*`.
    platform_context: Option<platform::PlatformContext>,
    content_loaded: bool,
    /// Unique attachment allocation. The public pointer is borrowed identity;
    /// only this slot owns and may drop the Box.
    active_attachment: Option<Box<surface::MigoSurfaceAttachment>>,
    /// Highest public generation accepted for this Session. Zero means no
    /// attachment has ever been accepted.
    last_public_generation: u64,
    /// Serializes cold-path attach/update/detach work while SessionControl is
    /// deliberately unlocked around platform/engine calls.
    surface_transition: SurfaceTransition,
    /// Public generation reserved by the current attach/update/detach. Keeping
    /// it explicit lets a renderer-side loss be correlated even before an
    /// attaching handle has been installed in `active_attachment`.
    surface_transition_generation: Option<u64>,
    /// First unexpected loss observed while attach/update owns the control
    /// transition. Applied exactly once when that transition commits or rolls
    /// back; detach intentionally suppresses its own retirement signal.
    pending_surface_loss: Option<PendingSurfaceLoss>,
    /// Immutable callback router created with the first Host. It owns only a
    /// Weak Session reference, so this is not a reference cycle.
    notifier: Option<Arc<callbacks::Notifier>>,
    /// Dynamic physical window metrics shared with the C HostKit. Retained by
    /// the Session so detach/reattach updates the same service object.
    window_state: Option<Arc<HostWindowState>>,
    /// Last visibility level set by the host.
    ///
    /// Visibility is a property of the session, not of the surface, and every
    /// Android lifecycle delivers RESUME before the window arrives. Rejecting
    /// the call until something is attached would make correct hosts look
    /// wrong; the value is remembered and applied when the surface lands.
    visibility: Option<bool>,
    /// Last focus level. Like visibility, this is a Session property retained
    /// across native Surface replacement.
    focused: Option<bool>,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            lifecycle: LifecycleState::Created,
            callbacks: None,
            callbacks_configured: false,
            callbacks_frozen: false,
            host: None,
            platform_identity: None,
            platform_context: None,
            content_loaded: false,
            active_attachment: None,
            last_public_generation: 0,
            surface_transition: SurfaceTransition::Idle,
            surface_transition_generation: None,
            pending_surface_loss: None,
            notifier: None,
            window_state: None,
            visibility: None,
            focused: None,
        }
    }
}

pub struct MigoSession {
    engine: Arc<EngineInner>,
    state: Mutex<SessionState>,
    /// Logical lifetime and callback admission. The raw C handle owns one Arc
    /// strong reference; every entry point and queued callback pins another.
    /// Destruction closes this gate and drains callbacks that already started
    /// on other threads before the raw handle is consumed.
    callback_gate: callback_gate::CallbackGate,
    /// Release observers do not retain the Session, so completion updates this
    /// separate counter through an Arc without taking SessionControl.
    pending_surface_releases: Arc<AtomicUsize>,
    /// The 128-bit identity an external producer's packets must carry, from the
    /// session configuration. `None` when the host supplied none, which is the
    /// ordinary case for a session that runs its own JavaScript.
    ///
    /// Immutable for the session's life: it is the value a transport was paired
    /// with out of band, and a session that could change it mid-flight would be
    /// a session whose accepted packets stop meaning the same thing.
    launch_nonce: Option<u128>,
    /// Installed exactly once after the first Host reaches its ready boundary.
    /// Hot input/vsync calls read these cloned senders without a registry lock.
    ingress: OnceLock<migo_core::HostIngress>,
    /// Public generation currently accepting data-plane events; zero means no
    /// attached Surface. Release/Acquire pairs detach with hot callers.
    active_surface_generation: AtomicU64,
    /// Connected gamepad topology packed per stable Web index. Publication is
    /// ordered after successful command enqueue.
    gamepad_topology: gamepad::GamepadTopology,
    /// Suppress repeated host notifications while bounded input ingress stays
    /// saturated. The next successful input enqueue opens a new episode.
    input_saturation_reported: AtomicBool,
}

impl MigoSession {
    #[inline]
    pub(crate) fn is_open(&self) -> bool {
        self.callback_gate.is_open()
    }

    #[inline]
    pub(crate) fn active_ingress(&self) -> Result<&migo_core::HostIngress, MigoResult> {
        if self.active_surface_generation.load(Ordering::Acquire) == 0 {
            return Err(MIGO_ERROR_INVALID_STATE);
        }
        self.ingress.get().ok_or(MIGO_ERROR_INTERNAL)
    }

    /// Linearize an unexpected renderer-side Surface loss against host detach.
    ///
    /// Both paths take `SessionControl`: whichever reserves the attachment
    /// first owns the observable outcome. A detach already in progress
    /// suppresses loss notification; a loss that wins closes data admission and
    /// leaves the handle available only for release.
    pub(crate) fn mark_surface_lost(
        &self,
        public_generation: u64,
        reason: SurfaceLossReason,
    ) -> bool {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(error) => error.into_inner(),
        };
        match state.surface_transition {
            SurfaceTransition::Detaching => return false,
            SurfaceTransition::Attaching | SurfaceTransition::Updating => {
                if state.surface_transition_generation != Some(public_generation) {
                    return false;
                }
                if state.pending_surface_loss.is_none() {
                    state.pending_surface_loss = Some(PendingSurfaceLoss {
                        generation: public_generation,
                        reason,
                    });
                }
                return false;
            }
            SurfaceTransition::Idle => {
                debug_assert!(state.surface_transition_generation.is_none());
                debug_assert!(state.pending_surface_loss.is_none());
            }
        }
        let Some(active) = state.active_attachment.as_mut() else {
            return false;
        };
        if active.public_generation() != public_generation || active.is_lost() {
            return false;
        }
        match self.active_surface_generation.compare_exchange(
            public_generation,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                active.mark_lost();
                true
            }
            Err(_) => false,
        }
    }
}

#[inline]
pub(crate) fn map_ingress_result<T>(
    session: &Arc<MigoSession>,
    entry: &'static str,
    result: Result<T, migo_core::HostIngressSendError>,
) -> MigoResult {
    match result {
        Ok(_) => {
            session
                .input_saturation_reported
                .store(false, Ordering::Release);
            MIGO_OK
        }
        Err(migo_core::HostIngressSendError::Full) => {
            tracing::debug!("{entry}: bounded input transport is saturated");
            if session
                .input_saturation_reported
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                let notifier = session
                    .state
                    .lock()
                    .map(|state| state.notifier.clone())
                    .unwrap_or_else(|error| error.into_inner().notifier.clone());
                if let Some(notifier) = notifier {
                    notifier.error(
                        MIGO_ERROR_WOULD_BLOCK,
                        format!(
                            "{entry}: bounded input transport saturated; event was not accepted"
                        ),
                    );
                }
            }
            MIGO_ERROR_WOULD_BLOCK
        }
        Err(migo_core::HostIngressSendError::Closed) => {
            tracing::debug!("{entry}: Host ingress is closed");
            MIGO_ERROR_INVALID_STATE
        }
    }
}

/// Temporarily pin a live raw Session handle for one ABI call.
///
/// As with every C handle API, the caller must not race destruction with a new
/// call through the same raw pointer. Reentrant destruction is safe because an
/// already-entered call or callback owns a strong pin until it returns.
///
/// # Safety
/// `session` is null or is the still-owned raw pointer produced by
/// `Arc::into_raw` in [`migo_session_create`].
pub(crate) unsafe fn pin_session(
    session: *mut MigoSession,
) -> Result<Arc<MigoSession>, MigoResult> {
    if session.is_null() {
        return Err(MIGO_ERROR_INVALID_ARGUMENT);
    }
    // SAFETY: guaranteed by the function contract. The increment is balanced
    // by the Arc returned from `from_raw`.
    unsafe { Arc::increment_strong_count(session.cast_const()) };
    let pinned = unsafe { Arc::from_raw(session.cast_const()) };
    if !pinned.is_open() {
        return Err(MIGO_ERROR_INVALID_STATE);
    }
    Ok(pinned)
}

// ---- Engine -----------------------------------------------------------------

/// # Safety
/// `config` must point to a `MigoEngineConfig`, `out_engine` to writable
/// storage for one pointer. Both are borrowed for the call only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_engine_create(
    config: *const MigoEngineConfig,
    out_engine: *mut *mut MigoEngine,
) -> MigoResult {
    guard("migo_engine_create", || {
        let Some(out_engine) = (unsafe { out_engine.as_mut() }) else {
            return MIGO_ERROR_INVALID_ARGUMENT;
        };
        let config = match unsafe { MigoEngineConfig::parse(config) } {
            Ok(config) => config,
            Err(error) => return error,
        };

        // The host owns the layout; we only make sure the roots it named exist
        // before the engine starts writing under them. The path string itself
        // was already validated by MigoEngineConfig::parse above, so a
        // failure here is the filesystem refusing a well-formed path --
        // permissions, a read-only mount, a path component that is a file --
        // not a malformed argument, hence INTERNAL rather than
        // INVALID_ARGUMENT.
        for dir in [&config.files_dir, &config.cache_dir, &config.code_cache_dir] {
            if let Err(error) = std::fs::create_dir_all(dir) {
                tracing::error!("migo_engine_create: cannot create {dir:?}: {error}");
                return MIGO_ERROR_INTERNAL;
            }
        }

        init_dev_logging();

        let engine = Box::new(MigoEngine {
            inner: Arc::new(EngineInner {
                files_dir: PathBuf::from(config.files_dir),
                cache_dir: PathBuf::from(config.cache_dir),
                code_cache_dir: PathBuf::from(config.code_cache_dir),
                allow_unsigned_content: config.allow_unsigned_content,
                live_sessions: Mutex::new(0),
                retired_hosts: retirement::RetirementSet::new(),
            }),
        });
        *out_engine = Box::into_raw(engine);
        MIGO_OK
    })
}

/// # Safety
/// `engine` must be a handle from [`migo_engine_create`] that has not been
/// destroyed. On `MIGO_OK` the pointer is consumed and invalid afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_engine_destroy(engine: *mut MigoEngine) -> MigoResult {
    guard("migo_engine_destroy", || {
        let Some(engine_ref) = (unsafe { engine.as_ref() }) else {
            return MIGO_ERROR_INVALID_ARGUMENT;
        };
        // Refuse rather than free configuration that live sessions still read.
        match engine_ref.inner.live_sessions.lock() {
            Ok(live) if *live > 0 => return MIGO_ERROR_INVALID_STATE,
            Ok(_) => {}
            Err(_) => return MIGO_ERROR_INTERNAL,
        }

        let retired_hosts = match engine_ref.inner.retired_hosts.take() {
            Ok(hosts) => hosts,
            Err(()) => {
                tracing::error!(
                    "migo_engine_destroy: cannot join a retired Host from that Host thread"
                );
                return MIGO_ERROR_INVALID_STATE;
            }
        };
        let mut join_failed = false;
        for mut host in retired_hosts {
            if let Err(error) = host.join() {
                tracing::error!("migo_engine_destroy: {error}");
                join_failed = true;
            }
        }
        if join_failed {
            return MIGO_ERROR_INTERNAL;
        }
        drop(unsafe { Box::from_raw(engine) });
        MIGO_OK
    })
}

// ---- Session ----------------------------------------------------------------

/// # Safety
/// `engine` must be a live engine handle; `config` a `MigoSessionConfig`;
/// `out_session` writable storage for one pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_create(
    engine: *mut MigoEngine,
    config: *const MigoSessionConfig,
    out_session: *mut *mut MigoSession,
) -> MigoResult {
    guard("migo_session_create", || {
        let Some(engine) = (unsafe { engine.as_ref() }) else {
            return MIGO_ERROR_INVALID_ARGUMENT;
        };
        let Some(out_session) = (unsafe { out_session.as_mut() }) else {
            return MIGO_ERROR_INVALID_ARGUMENT;
        };
        let validated = match unsafe { MigoSessionConfig::parse(config) } {
            Ok(validated) => validated,
            Err(error) => return error,
        };

        match engine.inner.live_sessions.lock() {
            Ok(mut live) => *live += 1,
            Err(_) => return MIGO_ERROR_INTERNAL,
        }
        let session = Arc::new(MigoSession {
            engine: Arc::clone(&engine.inner),
            state: Mutex::new(SessionState::default()),
            launch_nonce: validated.launch_nonce,
            callback_gate: callback_gate::CallbackGate::new(),
            pending_surface_releases: Arc::new(AtomicUsize::new(0)),
            ingress: OnceLock::new(),
            active_surface_generation: AtomicU64::new(0),
            gamepad_topology: gamepad::GamepadTopology::new(),
            input_saturation_reported: AtomicBool::new(false),
        });
        *out_session = Arc::into_raw(session).cast_mut();
        MIGO_OK
    })
}

/// # Safety
/// `session` must be a live session handle. On `MIGO_OK` it is consumed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_destroy(session: *mut MigoSession) -> MigoResult {
    guard("migo_session_destroy", || {
        let pinned = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };

        // Acquire every fallible lock and validate every destroy guard before
        // the logical/raw ownership boundary. A rejected destroy changes
        // nothing and remains retryable.
        let Ok(mut live_sessions) = pinned.engine.live_sessions.lock() else {
            return MIGO_ERROR_INTERNAL;
        };
        let Ok(mut state) = pinned.state.lock() else {
            return MIGO_ERROR_INTERNAL;
        };
        if state.surface_transition != SurfaceTransition::Idle
            || state.active_attachment.is_some()
            || pinned.pending_surface_releases.load(Ordering::Acquire) != 0
        {
            return MIGO_ERROR_INVALID_STATE;
        }
        if !platform_state_is_consistent(
            state.host.is_some(),
            state.platform_identity.is_some(),
            state.platform_context.is_some(),
        ) {
            tracing::error!("migo_session_destroy: inconsistent committed platform state");
            return MIGO_ERROR_INTERNAL;
        }
        if *live_sessions == 0 {
            return MIGO_ERROR_INTERNAL;
        }

        // The successful close is the logical destruction boundary. It rejects
        // queued/later callbacks and new ABI operations. A concurrent second
        // destroy can have passed `pin_session` before this boundary, so close
        // is checked again while the Session state is serialized.
        let callback_drain = match pinned.callback_gate.close() {
            Ok(drain) => drain,
            Err(_) => return MIGO_ERROR_INVALID_STATE,
        };
        let host = state.host.take();
        state.platform_identity.take();
        let platform_context = state.platform_context.take();
        state.notifier.take();
        if let Some(host) = host {
            // Publish ownership before the live count reaches zero. An Engine
            // destroy racing this call can therefore never miss an exiting
            // Host after it observes that no Session remains.
            pinned.engine.retired_hosts.retire(host);
        }
        *live_sessions -= 1;
        drop(state);
        drop(live_sessions);
        drop(platform_context);

        // Never wait while holding a Migo lock: an already-started callback may
        // be finishing an ABI call. Callback frames on this thread are exempt,
        // which preserves documented reentrant destruction.
        callback_drain.wait();

        // Consume exactly the one strong reference exported as the C handle.
        // `pinned` (and any currently executing callback) keeps the allocation
        // alive until its own stack has unwound.
        drop(unsafe { Arc::from_raw(session.cast_const()) });
        MIGO_OK
    })
}

/// # Safety
/// `session` must be live; `content` a `MigoContentDescriptor` borrowed for the
/// call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_load_content(
    session: *mut MigoSession,
    content: *const MigoContentDescriptor,
) -> MigoResult {
    guard("migo_session_load_content", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        let content = match unsafe { MigoContentDescriptor::parse(content) } {
            Ok(content) => content,
            Err(error) => return error,
        };

        let Ok(mut state) = session.state.lock() else {
            return MIGO_ERROR_INTERNAL;
        };
        // Content needs a render target: without one there is no host thread to
        // evaluate it on.
        let Some(host) = state.host.as_ref().map(SessionEngine::id) else {
            return MIGO_ERROR_INVALID_STATE;
        };
        if state.content_loaded {
            return MIGO_ERROR_INVALID_STATE;
        }

        match send_command_to_host(
            host,
            HostCommand::EvaluateModule {
                game_id: content.content_id,
                entry: content.entry,
            },
        ) {
            Ok(()) => {
                state.content_loaded = true;
                MIGO_OK
            }
            Err(error) => {
                tracing::error!("migo_session_load_content: {error}");
                MIGO_ERROR_INTERNAL
            }
        }
    })
}

/// # Safety
/// `session` must be live; `callbacks` a `MigoHostCallbacks` borrowed for the
/// call. Only the fields it declares are copied.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_set_host_callbacks(
    session: *mut MigoSession,
    callbacks: *const callbacks::MigoHostCallbacks,
) -> MigoResult {
    guard("migo_session_set_host_callbacks", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        let copied = match unsafe { callbacks::MigoHostCallbacks::parse(callbacks) } {
            Ok(copied) => copied,
            Err(error) => return error,
        };

        let Ok(mut state) = session.state.lock() else {
            return MIGO_ERROR_INTERNAL;
        };
        // Install-once, before the first attach: a second set would let tasks
        // already queued against the old pointers run against the new ones.
        if state.callbacks_configured || state.callbacks_frozen || state.host.is_some() {
            return MIGO_ERROR_INVALID_STATE;
        }
        state.callbacks = copied;
        state.callbacks_configured = true;
        MIGO_OK
    })
}

/// Lifecycle states from `include/migo/session.h`.
const MIGO_LIFECYCLE_CREATED: u32 = 0;
const MIGO_LIFECYCLE_RUNNING: u32 = 1;
const MIGO_LIFECYCLE_PAUSED: u32 = 2;

/// Drive the engine's show/hide channel — the same one Android's `onShow` /
/// `onHide` use, so a desktop host produces the lifecycle the content already
/// expects instead of a second, divergent notion of "paused".
///
/// `send_critical_command_to_host` matches Android: lifecycle must not be
/// dropped when the command queue is saturated.
/// Report that a frame boundary arrived, in response to `on_request_frame`.
///
/// # Safety
/// `session` must be live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_notify_vsync(
    session: *mut MigoSession,
    frame_time_nanos: i64,
) -> MigoResult {
    guard("migo_session_notify_vsync", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        if frame_time_nanos < 0 {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        let ingress = match session.active_ingress() {
            Ok(ingress) => ingress,
            Err(error) => return error,
        };
        // The engine measures frame time in milliseconds; the host reports the
        // platform's nanosecond timestamp, so the conversion belongs here
        // rather than in every host.
        match ingress.try_send_vsync(frame_time_nanos as f64 / 1_000_000.0) {
            // Frame ticks are level-like and the channel retains an earlier
            // pending tick. Saturation therefore coalesces instead of asking a
            // host to retry an obsolete timestamp.
            Ok(()) | Err(migo_core::HostIngressSendError::Full) => MIGO_OK,
            Err(migo_core::HostIngressSendError::Closed) => MIGO_ERROR_INVALID_STATE,
        }
    })
}

fn drive_visibility_locked(state: &mut SessionState, visible: bool) -> MigoResult {
    // Before a surface exists (including an in-progress attach/detach) there
    // is nothing to show or hide yet, but the host is not wrong to have said
    // so: remember it for the next completed attach.
    let can_dispatch = state.active_attachment.is_some()
        && state.surface_transition != SurfaceTransition::Detaching;
    let Some(host) = state
        .host
        .as_ref()
        .filter(|_| can_dispatch)
        .map(SessionEngine::id)
    else {
        state.visibility = Some(visible);
        return MIGO_OK;
    };
    let command = if visible {
        HostCommand::OnShow { options_json: None }
    } else {
        HostCommand::OnHide
    };
    match migo_core::send_critical_command_to_host(host, command) {
        Ok(()) => {
            state.visibility = Some(visible);
            MIGO_OK
        }
        Err(error) => {
            tracing::error!("lifecycle command failed: {error}");
            MIGO_ERROR_INTERNAL
        }
    }
}

fn drive_visibility(session: &MigoSession, visible: bool) -> MigoResult {
    let Ok(mut state) = session.state.lock() else {
        return MIGO_ERROR_INTERNAL;
    };
    drive_visibility_locked(&mut state, visible)
}

/// # Safety
/// `session` must be a live session handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_set_lifecycle(
    session: *mut MigoSession,
    state: u32,
) -> MigoResult {
    guard("migo_session_set_lifecycle", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        let desired = match state {
            MIGO_LIFECYCLE_RUNNING => LifecycleState::Running,
            MIGO_LIFECYCLE_PAUSED => LifecycleState::Paused,
            // CREATED is where a session starts; asking to go back to it would
            // mean unwinding a running engine, which the ABI does not define.
            MIGO_LIFECYCLE_CREATED => return MIGO_ERROR_INVALID_STATE,
            _ => return MIGO_ERROR_INVALID_ARGUMENT,
        };
        let Ok(mut control) = session.state.lock() else {
            return MIGO_ERROR_INTERNAL;
        };
        if control.lifecycle == desired {
            return MIGO_OK;
        }
        if desired == LifecycleState::Running {
            // Freeze before enqueueing RUNNING work: once engine activity can
            // produce callbacks, replacing callback tokens is unsafe.
            control.callbacks_frozen = true;
        }
        let result = drive_visibility_locked(&mut control, desired == LifecycleState::Running);
        if result == MIGO_OK {
            control.lifecycle = desired;
        }
        result
    })
}

/// # Safety
/// `session` must be a live session handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_set_visibility(
    session: *mut MigoSession,
    visible: u8,
) -> MigoResult {
    guard("migo_session_set_visibility", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        let visible = match migo_capi_abi::validate::validate_bool(visible) {
            Ok(visible) => visible,
            Err(error) => return error,
        };
        drive_visibility(&session, visible)
    })
}

/// # Safety
/// `session` must be a live session handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_set_focus(
    session: *mut MigoSession,
    focused: u8,
) -> MigoResult {
    guard("migo_session_set_focus", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        let focused = match migo_capi_abi::validate::validate_bool(focused) {
            Ok(focused) => focused,
            Err(error) => return error,
        };
        let Ok(mut state) = session.state.lock() else {
            return MIGO_ERROR_INTERNAL;
        };
        if state.focused == Some(focused) {
            return MIGO_OK;
        }

        // No surface requirement: focus is a Session property, and Android can
        // report it before its window exists. Retain it for attach in that
        // case. It never drives show/hide, render pause, or audio pause.
        let can_dispatch = state.active_attachment.is_some()
            && state.surface_transition != SurfaceTransition::Detaching;
        let Some(host) = state
            .host
            .as_ref()
            .filter(|_| can_dispatch)
            .map(SessionEngine::id)
        else {
            state.focused = Some(focused);
            return MIGO_OK;
        };
        match migo_core::send_critical_command_to_host(
            host,
            HostCommand::OnFocusChanged { focused },
        ) {
            Ok(()) => {
                state.focused = Some(focused);
                MIGO_OK
            }
            Err(error) => {
                tracing::error!("focus command failed: {error}");
                MIGO_ERROR_INTERNAL
            }
        }
    })
}

// ---- Development logging ----------------------------------------------------

/// The level `MIGO_CAPI_LOG` asks for, if a host set it.
///
/// Read in one place because it has two consumers that have to agree: the
/// subscriber this process installs, and the level every session hands its own
/// threads. An unrecognised value means Info rather than nothing, since a host
/// that set the variable at all is asking for output.
fn dev_log_level() -> Option<shared::config::LogLevel> {
    use shared::config::LogLevel;
    let level = std::env::var_os("MIGO_CAPI_LOG")?;
    Some(match level.to_string_lossy().to_lowercase().as_str() {
        "trace" => LogLevel::Trace,
        "debug" => LogLevel::Debug,
        "warn" => LogLevel::Warn,
        "error" => LogLevel::Error,
        _ => LogLevel::Info,
    })
}

/// Install a log subscriber when `MIGO_CAPI_LOG` is set.
///
/// A library has no business hijacking the process's global logger, so this is
/// opt-in and off by default. Hosts receive operational errors through
/// `on_error`; this switch exists only for additional engine diagnostics.
fn init_dev_logging() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let Some(level) = dev_log_level() else {
            return;
        };
        crate::platform::install_dev_logging(level);
        // A positive control, and it is here because its absence is the thing
        // that keeps being misread. Three CI iterations have now turned on this
        // switch to look for one specific warning, found nothing, and had no way
        // to tell "the engine had nothing to report" from "the variable never
        // reached this process" -- which on the iOS simulator is a real
        // possibility, because a test host runs inside the simulator and does not
        // inherit the runner's environment.
        //
        // One line, at the level the switch itself selected, so it appears
        // wherever the diagnostics being looked for would appear. If it is
        // missing, nothing else from the engine was ever going to be there.
        tracing::warn!(
            ?level,
            "migo engine diagnostics installed from MIGO_CAPI_LOG"
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        platform::test_platform_context,
        test_support::{
            callback_session_pin, engine_config, scratch_dirs, session_config, with_engine,
        },
    };
    use graphics::egl_platform::GraphicsBackendId;
    use migo_capi_abi::{MIGO_ABI_VERSION_CURRENT, MIGO_ERROR_UNSUPPORTED_ABI, VersionedHeader};
    use shared::host_channel::InputSendOutcome;
    use std::{
        ffi::{CString, c_void},
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc,
        },
        thread,
    };

    struct DropSentinel(Arc<AtomicBool>);
    struct TestPlatformBackend;
    struct TestPlatformDomain;

    fn install_test_host(state: &mut SessionState, host: SessionEngine) {
        state.host = Some(host);
        state.platform_identity = Some(PlatformIdentity::new::<TestPlatformDomain>(
            GraphicsBackendId::of::<TestPlatformBackend>(),
            0,
        ));
        state.platform_context = Some(test_platform_context());
    }

    impl Drop for DropSentinel {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[test]
    fn input_saturation_callback_is_once_per_episode() {
        static CALLBACKS: AtomicUsize = AtomicUsize::new(0);
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

        unsafe extern "C" fn dispatch(
            _dispatcher: *mut c_void,
            task: callbacks::MigoTaskFn,
            context: *mut c_void,
        ) -> MigoResult {
            unsafe { task(context) };
            MIGO_OK
        }

        unsafe extern "C" fn on_error(
            _user: *mut c_void,
            _session: *mut c_void,
            error: *const callbacks::MigoError,
        ) {
            assert_eq!(unsafe { &*error }.code, MIGO_ERROR_WOULD_BLOCK);
            CALLBACKS.fetch_add(1, Ordering::SeqCst);
        }

        let _serial = LOCK.lock().unwrap();
        CALLBACKS.store(0, Ordering::SeqCst);
        let session = callback_session_pin();
        let host_callbacks = callbacks::HostCallbacks {
            user_data: std::ptr::null_mut(),
            dispatcher_data: std::ptr::null_mut(),
            dispatch,
            on_ready: None,
            on_error: Some(on_error),
            on_exit_requested: None,
            on_surface_lost: None,
            on_request_frame: None,
            on_show_keyboard: None,
            on_hide_keyboard: None,
            on_update_keyboard: None,
            on_surface_released: None,
        };
        session.state.lock().unwrap().notifier = Some(Arc::new(callbacks::Notifier::new(
            host_callbacks,
            Arc::downgrade(&session),
        )));

        let full = || Err::<InputSendOutcome, _>(migo_core::HostIngressSendError::Full);
        assert_eq!(
            map_ingress_result(&session, "test_input", full()),
            MIGO_ERROR_WOULD_BLOCK
        );
        assert_eq!(
            map_ingress_result(&session, "test_input", full()),
            MIGO_ERROR_WOULD_BLOCK
        );
        assert_eq!(CALLBACKS.load(Ordering::SeqCst), 1);

        assert_eq!(
            map_ingress_result(&session, "test_input", Ok(InputSendOutcome::Coalesced),),
            MIGO_OK
        );
        assert_eq!(
            map_ingress_result(&session, "test_input", full()),
            MIGO_ERROR_WOULD_BLOCK
        );
        assert_eq!(CALLBACKS.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn engine_rejects_a_config_from_a_different_abi() {
        let dirs = scratch_dirs("abi");
        let config = engine_config(
            &dirs,
            size_of::<MigoEngineConfig>() as u32,
            MIGO_ABI_VERSION_CURRENT + 1,
        );
        let mut engine: *mut MigoEngine = std::ptr::null_mut();
        assert_eq!(
            unsafe { migo_engine_create(&config, &mut engine) },
            MIGO_ERROR_UNSUPPORTED_ABI
        );
        assert!(engine.is_null(), "no handle may escape a rejected call");
    }

    #[test]
    fn unsigned_content_is_refused_unless_the_host_opts_in() {
        // The signing check is the default; a host that wants unsigned content
        // has to say so, because silently accepting it defeats the check.
        let dirs = scratch_dirs("signing");
        let mut config = engine_config(
            &dirs,
            size_of::<MigoEngineConfig>() as u32,
            MIGO_ABI_VERSION_CURRENT,
        );
        let mut engine: *mut MigoEngine = std::ptr::null_mut();
        assert_eq!(unsafe { migo_engine_create(&config, &mut engine) }, MIGO_OK);
        assert!(
            !unsafe { &*engine }.inner.allow_unsigned_content,
            "MIGO_ENGINE_FLAG_NONE must keep signing enforced"
        );
        assert_eq!(unsafe { migo_engine_destroy(engine) }, MIGO_OK);

        config.flags = MIGO_ENGINE_FLAG_ALLOW_UNSIGNED_CONTENT;
        let mut engine: *mut MigoEngine = std::ptr::null_mut();
        assert_eq!(unsafe { migo_engine_create(&config, &mut engine) }, MIGO_OK);
        assert!(unsafe { &*engine }.inner.allow_unsigned_content);
        assert_eq!(unsafe { migo_engine_destroy(engine) }, MIGO_OK);
    }

    #[test]
    fn engine_requires_storage_roots() {
        let dirs = scratch_dirs("roots");
        let mut config = engine_config(
            &dirs,
            size_of::<MigoEngineConfig>() as u32,
            MIGO_ABI_VERSION_CURRENT,
        );
        config.files_dir_utf8 = std::ptr::null();
        let mut engine: *mut MigoEngine = std::ptr::null_mut();
        assert_eq!(
            unsafe { migo_engine_create(&config, &mut engine) },
            MIGO_ERROR_INVALID_ARGUMENT
        );
    }

    #[test]
    fn engine_reports_internal_when_a_storage_root_cannot_be_created() {
        // A regular file sitting where a path component needs to be a
        // directory forces create_dir_all to fail deterministically (ENOTDIR)
        // without needing filesystem-permission tricks. The path string
        // itself is well-formed, so this must not be reported as
        // MIGO_ERROR_INVALID_ARGUMENT -- that code means the argument was
        // malformed, and this one was not.
        let blocking_file = std::env::temp_dir().join(format!(
            "migo-capi-test-blocked-root-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&blocking_file);
        std::fs::write(&blocking_file, b"not a directory").expect("create blocking file");

        let make = |name: &str| {
            CString::new(blocking_file.join(name).to_str().expect("utf-8 path")).expect("cstring")
        };
        let dirs = (make("files"), make("cache"), make("code-cache"));
        let config = engine_config(
            &dirs,
            size_of::<MigoEngineConfig>() as u32,
            MIGO_ABI_VERSION_CURRENT,
        );
        let mut engine: *mut MigoEngine = std::ptr::null_mut();
        assert_eq!(
            unsafe { migo_engine_create(&config, &mut engine) },
            MIGO_ERROR_INTERNAL
        );
        assert!(engine.is_null(), "no handle may escape a rejected call");

        let _ = std::fs::remove_file(&blocking_file);
    }

    #[test]
    fn engine_creates_its_storage_roots() {
        let dirs = scratch_dirs("mkdir");
        let files = PathBuf::from(dirs.0.to_str().expect("utf-8"));
        let _ = std::fs::remove_dir_all(&files);
        with_engine("mkdir", |_| {});
        assert!(
            files.is_dir(),
            "engine must create the roots the host named"
        );
    }

    #[test]
    fn engine_refuses_to_die_while_sessions_are_live() {
        // Otherwise the session would keep reading configuration that just got
        // freed underneath it.
        let dirs = scratch_dirs("children");
        let config = engine_config(
            &dirs,
            size_of::<MigoEngineConfig>() as u32,
            MIGO_ABI_VERSION_CURRENT,
        );
        let mut engine: *mut MigoEngine = std::ptr::null_mut();
        assert_eq!(unsafe { migo_engine_create(&config, &mut engine) }, MIGO_OK);

        let session_config = session_config();
        let mut session: *mut MigoSession = std::ptr::null_mut();
        assert_eq!(
            unsafe { migo_session_create(engine, &session_config, &mut session) },
            MIGO_OK
        );
        assert_eq!(
            unsafe { migo_engine_destroy(engine) },
            MIGO_ERROR_INVALID_STATE
        );

        assert_eq!(unsafe { migo_session_destroy(session) }, MIGO_OK);
        assert_eq!(unsafe { migo_engine_destroy(engine) }, MIGO_OK);
    }

    #[test]
    fn engine_destroy_waits_for_retired_host_sentinel() {
        let dirs = scratch_dirs("engine-retired-host");
        let config = engine_config(
            &dirs,
            size_of::<MigoEngineConfig>() as u32,
            MIGO_ABI_VERSION_CURRENT,
        );
        let mut engine: *mut MigoEngine = std::ptr::null_mut();
        assert_eq!(unsafe { migo_engine_create(&config, &mut engine) }, MIGO_OK);

        let dropped = Arc::new(AtomicBool::new(false));
        let sentinel = DropSentinel(Arc::clone(&dropped));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let join = thread::Builder::new()
            .name("Migo-Main-capi-retired-host".to_owned())
            .spawn(move || {
                let _sentinel = sentinel;
                started_tx.send(()).expect("publish Host start");
                release_rx.recv().expect("release retired Host");
            })
            .expect("spawn retired Host");
        let host = crate::session_engine::engine_for_test(8_001, join);
        unsafe { &*engine }.inner.retired_hosts.retire(host);

        started_rx.recv().expect("retired Host started");
        let (destroyed_tx, destroyed_rx) = mpsc::channel();
        let engine_address = engine as usize;
        let destroyer = thread::spawn(move || {
            let engine = engine_address as *mut MigoEngine;
            destroyed_tx
                .send(unsafe { migo_engine_destroy(engine) })
                .expect("publish Engine destruction");
        });

        assert!(
            destroyed_rx.try_recv().is_err(),
            "Engine destruction returned while a retired Host was live"
        );
        assert!(!dropped.load(Ordering::Acquire));

        release_tx.send(()).expect("release retired Host");
        assert_eq!(destroyed_rx.recv().expect("Engine result"), MIGO_OK);
        destroyer.join().expect("Engine destroyer");
        assert!(dropped.load(Ordering::Acquire));
    }

    #[test]
    fn engine_destroy_from_its_retired_host_is_rejected_and_retryable() {
        let dirs = scratch_dirs("engine-self-join");
        let config = engine_config(
            &dirs,
            size_of::<MigoEngineConfig>() as u32,
            MIGO_ABI_VERSION_CURRENT,
        );
        let mut engine: *mut MigoEngine = std::ptr::null_mut();
        assert_eq!(unsafe { migo_engine_create(&config, &mut engine) }, MIGO_OK);

        let (owner_tx, owner_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let engine_address = engine as usize;
        let join = thread::Builder::new()
            .name("Migo-Main-capi-self-join".to_owned())
            .spawn(move || {
                let host = owner_rx.recv().expect("receive own Host owner");
                let engine = engine_address as *mut MigoEngine;
                unsafe { &*engine }.inner.retired_hosts.retire(host);
                result_tx
                    .send(unsafe { migo_engine_destroy(engine) })
                    .expect("publish self-join rejection");
            })
            .expect("spawn self-join Host");
        let host = crate::session_engine::engine_for_test(8_002, join);
        owner_tx.send(host).expect("transfer owner to Host");

        assert_eq!(
            result_rx.recv().expect("self-join result"),
            MIGO_ERROR_INVALID_STATE
        );
        assert_eq!(unsafe { migo_engine_destroy(engine) }, MIGO_OK);
    }

    #[test]
    fn session_destroy_transfers_host_without_joining_it() {
        let dirs = scratch_dirs("session-retire-host");
        let config = engine_config(
            &dirs,
            size_of::<MigoEngineConfig>() as u32,
            MIGO_ABI_VERSION_CURRENT,
        );
        let mut engine: *mut MigoEngine = std::ptr::null_mut();
        assert_eq!(unsafe { migo_engine_create(&config, &mut engine) }, MIGO_OK);
        let mut session: *mut MigoSession = std::ptr::null_mut();
        assert_eq!(
            unsafe { migo_session_create(engine, &session_config(), &mut session) },
            MIGO_OK
        );

        let dropped = Arc::new(AtomicBool::new(false));
        let sentinel = DropSentinel(Arc::clone(&dropped));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let join = thread::Builder::new()
            .name("Migo-Main-capi-session-retire".to_owned())
            .spawn(move || {
                let _sentinel = sentinel;
                started_tx.send(()).expect("publish Host start");
                release_rx.recv().expect("release retired Host");
            })
            .expect("spawn Session Host");
        install_test_host(
            &mut unsafe { &*session }.state.lock().expect("Session state"),
            crate::session_engine::engine_for_test(8_003, join),
        );
        started_rx.recv().expect("Session Host started");

        assert_eq!(unsafe { migo_session_destroy(session) }, MIGO_OK);
        assert!(
            !dropped.load(Ordering::Acquire),
            "Session destruction must not join its Host"
        );
        assert_eq!(
            unsafe { &*engine }
                .inner
                .live_sessions
                .lock()
                .expect("live Sessions")
                .to_owned(),
            0
        );
        assert_eq!(unsafe { &*engine }.inner.retired_hosts.len(), 1);

        release_tx.send(()).expect("release retired Host");
        assert_eq!(unsafe { migo_engine_destroy(engine) }, MIGO_OK);
        assert!(dropped.load(Ordering::Acquire));
    }

    #[test]
    fn rejected_session_destroy_preserves_host_ownership_for_retry() {
        let dirs = scratch_dirs("session-destroy-retry");
        let config = engine_config(
            &dirs,
            size_of::<MigoEngineConfig>() as u32,
            MIGO_ABI_VERSION_CURRENT,
        );
        let mut engine: *mut MigoEngine = std::ptr::null_mut();
        assert_eq!(unsafe { migo_engine_create(&config, &mut engine) }, MIGO_OK);
        let mut session: *mut MigoSession = std::ptr::null_mut();
        assert_eq!(
            unsafe { migo_session_create(engine, &session_config(), &mut session) },
            MIGO_OK
        );

        let (release_tx, release_rx) = mpsc::channel();
        let join = thread::Builder::new()
            .name("Migo-Main-capi-destroy-retry".to_owned())
            .spawn(move || {
                release_rx.recv().expect("release retry Host");
            })
            .expect("spawn retry Host");
        {
            let mut state = unsafe { &*session }.state.lock().expect("Session state");
            install_test_host(
                &mut state,
                crate::session_engine::engine_for_test(8_004, join),
            );
            state.surface_transition = SurfaceTransition::Attaching;
        }

        assert_eq!(
            unsafe { migo_session_destroy(session) },
            MIGO_ERROR_INVALID_STATE
        );
        assert_eq!(
            unsafe { &*session }
                .state
                .lock()
                .expect("Session state")
                .host
                .as_ref()
                .map(SessionEngine::id),
            Some(8_004)
        );
        assert_eq!(
            unsafe { &*engine }
                .inner
                .live_sessions
                .lock()
                .expect("live Sessions")
                .to_owned(),
            1
        );
        assert_eq!(unsafe { &*engine }.inner.retired_hosts.len(), 0);

        unsafe { &*session }
            .state
            .lock()
            .expect("Session state")
            .surface_transition = SurfaceTransition::Idle;
        assert_eq!(unsafe { migo_session_destroy(session) }, MIGO_OK);
        release_tx.send(()).expect("release retry Host");
        assert_eq!(unsafe { migo_engine_destroy(engine) }, MIGO_OK);
    }

    #[test]
    fn loading_content_before_a_surface_is_a_state_error() {
        // No surface means no host thread, so there is nothing to evaluate on.
        with_engine("content", |engine| {
            let session_config = session_config();
            let mut session: *mut MigoSession = std::ptr::null_mut();
            assert_eq!(
                unsafe { migo_session_create(engine, &session_config, &mut session) },
                MIGO_OK
            );

            let content_id = CString::new("demo").expect("cstring");
            let entry = CString::new("game.js").expect("cstring");
            let content = MigoContentDescriptor {
                header: VersionedHeader {
                    struct_size: size_of::<MigoContentDescriptor>() as u32,
                    abi_version: MIGO_ABI_VERSION_CURRENT,
                },
                flags: 0,
                reserved0: 0,
                content_id_utf8: content_id.as_ptr(),
                entry_utf8: entry.as_ptr(),
            };
            assert_eq!(
                unsafe { migo_session_load_content(session, &content) },
                MIGO_ERROR_INVALID_STATE
            );
            assert_eq!(unsafe { migo_session_destroy(session) }, MIGO_OK);
        });
    }

    #[test]
    fn lifecycle_calls_are_accepted_before_a_surface_exists() {
        // These describe the session, not the surface, and every Android
        // lifecycle delivers resume and focus before the window arrives.
        // Rejecting them there would make a correct host look wrong; the
        // visibility is remembered and applied when the surface attaches.
        with_engine("lifecycle", |engine| {
            let session_config = session_config();
            let mut session: *mut MigoSession = std::ptr::null_mut();
            assert_eq!(
                unsafe { migo_session_create(engine, &session_config, &mut session) },
                MIGO_OK
            );
            assert_eq!(
                unsafe { migo_session_set_lifecycle(session, MIGO_LIFECYCLE_RUNNING) },
                MIGO_OK
            );
            assert_eq!(unsafe { migo_session_set_visibility(session, 1) }, MIGO_OK);
            assert_eq!(unsafe { migo_session_set_focus(session, 1) }, MIGO_OK);
            assert_eq!(unsafe { migo_session_destroy(session) }, MIGO_OK);
        });
    }

    #[test]
    fn lifecycle_state_and_session_levels_are_retained_without_a_surface() {
        with_engine("lifecycle-state", |engine| {
            let mut session = std::ptr::null_mut();
            assert_eq!(
                unsafe { migo_session_create(engine, &session_config(), &mut session) },
                MIGO_OK
            );

            // A process may pause before its first window is attached. The
            // level is valid and retained rather than inventing a Host command.
            assert_eq!(
                unsafe { migo_session_set_lifecycle(session, MIGO_LIFECYCLE_PAUSED) },
                MIGO_OK
            );
            {
                let state = unsafe { &*session }.state.lock().unwrap();
                assert_eq!(state.lifecycle, LifecycleState::Paused);
                assert_eq!(state.visibility, Some(false));
            }

            assert_eq!(
                unsafe { migo_session_set_lifecycle(session, MIGO_LIFECYCLE_RUNNING) },
                MIGO_OK
            );
            assert_eq!(unsafe { migo_session_set_focus(session, 1) }, MIGO_OK);
            {
                let state = unsafe { &*session }.state.lock().unwrap();
                assert_eq!(state.lifecycle, LifecycleState::Running);
                assert_eq!(state.visibility, Some(true));
                assert_eq!(state.focused, Some(true));
                assert!(state.callbacks_frozen);
            }

            // Repeating a level is idempotent and does not manufacture a
            // duplicate show/focus event.
            assert_eq!(
                unsafe { migo_session_set_lifecycle(session, MIGO_LIFECYCLE_RUNNING) },
                MIGO_OK
            );
            assert_eq!(unsafe { migo_session_set_focus(session, 1) }, MIGO_OK);
            assert_eq!(unsafe { migo_session_destroy(session) }, MIGO_OK);
        });
    }

    #[test]
    fn public_boolean_parameters_are_strict() {
        with_engine("strict-booleans", |engine| {
            let mut session = std::ptr::null_mut();
            assert_eq!(
                unsafe { migo_session_create(engine, &session_config(), &mut session) },
                MIGO_OK
            );
            for invalid in [2, u8::MAX] {
                assert_eq!(
                    unsafe { migo_session_set_visibility(session, invalid) },
                    MIGO_ERROR_INVALID_ARGUMENT
                );
                assert_eq!(
                    unsafe { migo_session_set_focus(session, invalid) },
                    MIGO_ERROR_INVALID_ARGUMENT
                );
            }
            assert_eq!(unsafe { migo_session_destroy(session) }, MIGO_OK);
        });
    }

    #[test]
    fn an_entered_call_pins_the_arc_allocation_across_reentrant_destroy() {
        with_engine("session-arc-pin", |engine| {
            let mut session = std::ptr::null_mut();
            assert_eq!(
                unsafe { migo_session_create(engine, &session_config(), &mut session) },
                MIGO_OK,
            );
            let entered = unsafe { pin_session(session) }.expect("live Session pin");
            assert!(entered.is_open());

            assert_eq!(unsafe { migo_session_destroy(session) }, MIGO_OK);
            assert!(
                !entered.is_open(),
                "logical close must be visible before destroy returns"
            );
            assert_eq!(
                Arc::strong_count(&entered),
                1,
                "the entered call is now the only allocation owner"
            );
        });
    }

    #[test]
    fn returning_to_the_created_state_is_still_rejected() {
        // Unlike the ordering cases above, this is a genuinely undefined
        // transition: unwinding a running engine is not something the ABI
        // describes, so it stays an error rather than becoming a deferral.
        with_engine("lifecycle-created", |engine| {
            let session_config = session_config();
            let mut session: *mut MigoSession = std::ptr::null_mut();
            assert_eq!(
                unsafe { migo_session_create(engine, &session_config, &mut session) },
                MIGO_OK
            );
            assert_eq!(
                unsafe { migo_session_set_lifecycle(session, MIGO_LIFECYCLE_CREATED) },
                MIGO_ERROR_INVALID_STATE
            );
            assert_eq!(unsafe { migo_session_destroy(session) }, MIGO_OK);
        });
    }

    #[test]
    fn a_vsync_without_a_surface_is_a_state_error() {
        // Unlike the session-level calls, a frame tick has nothing to pace
        // until something is rendering, and reporting success would tell the
        // host its tick was consumed.
        with_engine("vsync-detached", |engine| {
            let session_config = session_config();
            let mut session: *mut MigoSession = std::ptr::null_mut();
            assert_eq!(
                unsafe { migo_session_create(engine, &session_config, &mut session) },
                MIGO_OK
            );
            assert_eq!(
                unsafe { migo_session_notify_vsync(session, 1_000_000) },
                MIGO_ERROR_INVALID_STATE
            );
            assert_eq!(unsafe { migo_session_destroy(session) }, MIGO_OK);
        });
    }

    #[test]
    fn a_negative_vsync_timestamp_is_an_argument_error() {
        with_engine("vsync-negative", |engine| {
            let mut session = std::ptr::null_mut();
            assert_eq!(
                unsafe { migo_session_create(engine, &session_config(), &mut session) },
                MIGO_OK
            );
            assert_eq!(
                unsafe { migo_session_notify_vsync(session, -1) },
                MIGO_ERROR_INVALID_ARGUMENT
            );
            assert_eq!(unsafe { migo_session_destroy(session) }, MIGO_OK);
        });
    }

    #[test]
    fn unknown_lifecycle_states_are_told_apart_from_unsupported_transitions() {
        // A value outside the enum is a bad argument; going back to CREATED is
        // a defined value the ABI simply does not support.
        with_engine("lifecycle-values", |engine| {
            let session_config = session_config();
            let mut session: *mut MigoSession = std::ptr::null_mut();
            assert_eq!(
                unsafe { migo_session_create(engine, &session_config, &mut session) },
                MIGO_OK
            );
            assert_eq!(
                unsafe { migo_session_set_lifecycle(session, 99) },
                MIGO_ERROR_INVALID_ARGUMENT
            );
            assert_eq!(
                unsafe { migo_session_set_lifecycle(session, MIGO_LIFECYCLE_CREATED) },
                MIGO_ERROR_INVALID_STATE
            );
            assert_eq!(unsafe { migo_session_destroy(session) }, MIGO_OK);
        });
    }

    #[test]
    fn callbacks_install_only_once() {
        // A second install would let tasks queued against the old pointers run
        // against the new ones.
        with_engine("callbacks-once", |engine| {
            let session_config = session_config();
            let mut session: *mut MigoSession = std::ptr::null_mut();
            assert_eq!(
                unsafe { migo_session_create(engine, &session_config, &mut session) },
                MIGO_OK
            );

            unsafe extern "C" fn dispatch(
                _dispatcher: *mut c_void,
                task: callbacks::MigoTaskFn,
                context: *mut c_void,
            ) -> MigoResult {
                unsafe { task(context) };
                MIGO_OK
            }
            unsafe extern "C" fn on_ready(_user: *mut c_void, _session: *mut c_void) {}

            let host_callbacks = callbacks::MigoHostCallbacks {
                header: VersionedHeader {
                    struct_size: size_of::<callbacks::MigoHostCallbacks>() as u32,
                    abi_version: MIGO_ABI_VERSION_CURRENT,
                },
                user_data: std::ptr::null_mut(),
                dispatcher_data: std::ptr::null_mut(),
                dispatch: Some(dispatch),
                on_ready: Some(on_ready),
                on_error: None,
                on_exit_requested: None,
                on_surface_lost: None,
                on_request_frame: None,
                on_show_keyboard: None,
                on_hide_keyboard: None,
                on_update_keyboard: None,
                on_surface_released: None,
            };
            assert_eq!(
                unsafe { migo_session_set_host_callbacks(session, &host_callbacks) },
                MIGO_OK
            );
            assert_eq!(
                unsafe { migo_session_set_host_callbacks(session, &host_callbacks) },
                MIGO_ERROR_INVALID_STATE
            );
            assert_eq!(unsafe { migo_session_destroy(session) }, MIGO_OK);
        });
    }

    #[test]
    fn an_empty_callback_set_is_valid_and_still_installs_only_once() {
        with_engine("callbacks-empty", |engine| {
            let session_config = session_config();
            let mut session: *mut MigoSession = std::ptr::null_mut();
            assert_eq!(
                unsafe { migo_session_create(engine, &session_config, &mut session) },
                MIGO_OK
            );

            let host_callbacks = callbacks::MigoHostCallbacks::empty();
            assert_eq!(
                unsafe { migo_session_set_host_callbacks(session, &host_callbacks) },
                MIGO_OK
            );
            assert_eq!(
                unsafe { migo_session_set_host_callbacks(session, &host_callbacks) },
                MIGO_ERROR_INVALID_STATE
            );
            assert_eq!(unsafe { migo_session_destroy(session) }, MIGO_OK);
        });
    }

    #[test]
    fn running_freezes_callback_configuration_before_surface_attach() {
        with_engine("callbacks-running", |engine| {
            let session_config = session_config();
            let mut session: *mut MigoSession = std::ptr::null_mut();
            assert_eq!(
                unsafe { migo_session_create(engine, &session_config, &mut session) },
                MIGO_OK
            );
            assert_eq!(
                unsafe { migo_session_set_lifecycle(session, MIGO_LIFECYCLE_RUNNING) },
                MIGO_OK
            );

            let host_callbacks = callbacks::MigoHostCallbacks::empty();
            assert_eq!(
                unsafe { migo_session_set_host_callbacks(session, &host_callbacks) },
                MIGO_ERROR_INVALID_STATE
            );
            assert_eq!(unsafe { migo_session_destroy(session) }, MIGO_OK);
        });
    }

    #[test]
    fn null_handles_are_argument_errors_not_crashes() {
        assert_eq!(
            unsafe { migo_engine_destroy(std::ptr::null_mut()) },
            MIGO_ERROR_INVALID_ARGUMENT
        );
        assert_eq!(
            unsafe { migo_session_destroy(std::ptr::null_mut()) },
            MIGO_ERROR_INVALID_ARGUMENT
        );
        assert_eq!(
            unsafe { migo_surface_begin_detach(std::ptr::null_mut(), std::ptr::null_mut()) },
            MIGO_ERROR_INVALID_ARGUMENT
        );
        assert_eq!(
            unsafe { migo_surface_release_query(std::ptr::null(), std::ptr::null_mut()) },
            MIGO_ERROR_INVALID_ARGUMENT
        );
        assert_eq!(
            unsafe { migo_surface_release_destroy(std::ptr::null_mut()) },
            MIGO_ERROR_INVALID_ARGUMENT
        );
    }
}
