//! Surface attach, update and detach.
//!
//! Split out of `lib.rs` because these three entry points, the descriptors they
//! read, and the attachment handle they hand back are one concern: they are the
//! only place the ABI touches a host's window.
//!
//! The engine is told about a resize the same way Android tells it -- a fresh
//! generation-tagged lease delivered as `HostCommand::UpdateSurface` (see
//! `platform/android/jni/inbound.rs`). Using the one path keeps a resize from
//! meaning different things on different platforms.

use std::{
    mem::MaybeUninit,
    ptr::NonNull,
    sync::{Arc, Weak},
};

#[cfg(test)]
use std::{mem::size_of, thread};

use graphics::egl_platform::PlatformIdentity;
use migo_core::{
    PlatformServices, lease_surface_tracked, lease_surface_with_resource, retire_surface,
    send_critical_command_to_host,
};
use shared::surface::{HostWindowMetrics, HostWindowState};
use shared::{
    protocol::host_cmd::HostCommand,
    surface::{
        PixelRatio, PublicSurfaceGeneration, SurfaceLossReason, SurfaceReleaseNotification,
        SurfaceReleaseObserver, SurfaceReleasePhase, SurfaceResourceLease,
    },
};

use crate::panic_barrier::guard;
use crate::{
    MigoSession, SurfaceTransition, callbacks, host_kit, pin_session,
    platform::{PlatformTarget, build_target, rebuild_surface, supported_platform_kinds},
    platform_state_is_consistent,
};
use migo_capi_abi::{
    MIGO_ERROR_INTERNAL, MIGO_ERROR_INVALID_ARGUMENT, MIGO_ERROR_INVALID_STATE,
    MIGO_ERROR_STALE_SURFACE, MIGO_OK, MigoResult,
};

pub use migo_capi_abi::surface::{
    MigoAndroidNativeWindowDescriptor, MigoSurfaceDescriptor, MigoSurfaceMetrics,
    MigoSurfaceReleaseStatus, MigoWaylandSurfaceDescriptor, MigoWin32HwndDescriptor,
    MigoX11WindowDescriptor,
};
use migo_capi_abi::surface::{
    SurfaceDescriptorRef, validate_attach_generation, validate_update_generation,
};

// ---- Attachment handle ------------------------------------------------------

pub struct MigoSurfaceAttachment {
    session: NonNull<MigoSession>,
    public_generation: PublicSurfaceGeneration,
    resource: SurfaceResourceLease,
    target: PlatformTarget,
    width_pixels: u32,
    height_pixels: u32,
    pixel_ratio: PixelRatio,
    window_state: Arc<HostWindowState>,
    lost: bool,
}

fn validate_platform_identity(
    existing_host: Option<migo_core::HostId>,
    existing_identity: Option<PlatformIdentity>,
    candidate_identity: PlatformIdentity,
) -> Result<(), MigoResult> {
    match (existing_host, existing_identity) {
        (None, None) => Ok(()),
        (Some(_), Some(identity)) if identity == candidate_identity => Ok(()),
        (Some(_), Some(_)) => Err(MIGO_ERROR_INVALID_STATE),
        (None, Some(_)) | (Some(_), None) => Err(MIGO_ERROR_INTERNAL),
    }
}

fn validate_platform_context_state(
    existing_host: Option<migo_core::HostId>,
    existing_identity: Option<PlatformIdentity>,
    has_context: bool,
) -> Result<(), MigoResult> {
    platform_state_is_consistent(
        existing_host.is_some(),
        existing_identity.is_some(),
        has_context,
    )
    .then_some(())
    .ok_or(MIGO_ERROR_INTERNAL)
}

// SAFETY: the self-Session pointer is opaque identity produced from the Arc
// allocation and is never dereferenced without first taking a strong Session
// pin. All mutable attachment state remains behind SessionControl, and the C
// contract requires calls through a unique attachment handle to be serialized.
unsafe impl Send for MigoSurfaceAttachment {}

impl MigoSurfaceAttachment {
    #[inline]
    pub(crate) fn public_generation(&self) -> u64 {
        self.public_generation.get()
    }

    #[inline]
    pub(crate) fn is_lost(&self) -> bool {
        self.lost
    }

    #[inline]
    pub(crate) fn mark_lost(&mut self) {
        self.lost = true;
    }
}

/// Level-triggered observer returned by `migo_surface_begin_detach`.
///
/// It intentionally owns no Surface resource lease and may therefore outlive
/// the Session whose pending counter it once contributed to.
pub struct MigoSurfaceRelease {
    observer: SurfaceReleaseObserver,
}

struct DeferredSurfaceLoss {
    notifier: Option<Arc<callbacks::Notifier>>,
    generation: u64,
    reason: SurfaceLossReason,
}

impl DeferredSurfaceLoss {
    fn notify(self) {
        if let Some(notifier) = self.notifier {
            notifier.surface_lost(self.generation, self.reason.as_u32());
        }
    }
}

/// Apply and consume a renderer loss held while attach/update owned the
/// Session's control transition. Caller holds SessionControl.
fn consume_pending_surface_loss(
    session: &MigoSession,
    state: &mut crate::SessionState,
) -> Option<DeferredSurfaceLoss> {
    let loss = state.pending_surface_loss.take()?;
    let active = state.active_attachment.as_mut()?;
    if active.public_generation() != loss.generation || active.is_lost() {
        return None;
    }
    active.mark_lost();
    session
        .active_surface_generation
        .store(0, std::sync::atomic::Ordering::Release);
    Some(DeferredSurfaceLoss {
        notifier: state.notifier.clone(),
        generation: loss.generation,
        reason: loss.reason,
    })
}

fn rollback_surface_transition(session: &MigoSession) {
    let deferred_loss = {
        // A failed pre-commit operation must always release its reservation, so
        // this reads the guard through poison rather than giving up on one.
        //
        // What that buys is narrower than it looks, and worth stating because the
        // wording here used to claim otherwise: recovering the data does **not**
        // clear the mutex's poison flag, and every other entry point takes the
        // guard with a plain `lock()` and answers MIGO_ERROR_INTERNAL when it
        // fails. So a Session whose lock was poisoned is finished either way; what
        // this preserves is that the recorded state still describes what is
        // installed, for the one reader that can see it and for anything a later
        // change makes poison-tolerant. `Mutex::clear_poison` would make the
        // Session usable again and is deliberately not called: poison means a
        // thread panicked mid-mutation, and nothing here knows which of
        // `active_attachment`, `host` and `platform_context` it had finished
        // writing, so declaring the invariants restored would be a guess.
        let mut state = session
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let deferred_loss = consume_pending_surface_loss(session, &mut state);
        state.surface_transition_generation = None;
        state.surface_transition = SurfaceTransition::Idle;
        deferred_loss
    };
    if let Some(loss) = deferred_loss {
        loss.notify();
    }
}

fn release_notification(
    notifier: Option<&Arc<callbacks::Notifier>>,
) -> Option<SurfaceReleaseNotification> {
    let notifier = notifier.filter(|notifier| notifier.notifies_surface_release())?;
    let notifier: Weak<callbacks::Notifier> = Arc::downgrade(notifier);
    Some(Box::new(move |generation| {
        if let Some(notifier) = notifier.upgrade() {
            notifier.surface_released(generation.get());
        }
    }))
}

// ---- Entry points -----------------------------------------------------------

/// # Safety
/// `session` must be live; `descriptor` a `MigoSurfaceDescriptor` whose
/// `platform_descriptor` points at the matching typed descriptor;
/// `out_attachment` writable storage for one pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_attach_surface(
    session: *mut MigoSession,
    descriptor: *const MigoSurfaceDescriptor,
    out_attachment: *mut *mut MigoSurfaceAttachment,
) -> MigoResult {
    guard("migo_session_attach_surface", || {
        let Some(out_attachment) = (unsafe { out_attachment.as_mut() }) else {
            return MIGO_ERROR_INVALID_ARGUMENT;
        };
        *out_attachment = std::ptr::null_mut();
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        let session_ptr = NonNull::from(Arc::as_ref(&session));
        let descriptor = match unsafe {
            SurfaceDescriptorRef::parse_for_platforms(descriptor, supported_platform_kinds())
        } {
            Ok(descriptor) => descriptor,
            Err(error) => return error,
        };
        let configuration = descriptor.configuration();
        let pixel_ratio = PixelRatio::new(configuration.scale_factor())
            .expect("the ABI validator rejects invalid scale factors");
        let window_metrics = HostWindowMetrics::new(
            configuration.width_pixels(),
            configuration.height_pixels(),
            pixel_ratio,
        );
        let public_generation = PublicSurfaceGeneration::new(descriptor.public_generation())
            .expect("the ABI validator rejects generation zero");

        let (
            existing_host,
            existing_platform_identity,
            existing_platform_context,
            configured_callbacks,
            existing_window_state,
        ) = {
            let Ok(mut state) = session.state.lock() else {
                // The one INTERNAL this function could return without saying
                // anything at all. Every other one logs; this one is first, so
                // it is the one a host hits before there is any other signal.
                tracing::error!("migo_session_attach_surface: the Session state lock is poisoned");
                return MIGO_ERROR_INTERNAL;
            };
            if state.surface_transition != SurfaceTransition::Idle
                || state.active_attachment.is_some()
            {
                return MIGO_ERROR_INVALID_STATE;
            }
            if let Err(error) = validate_attach_generation(
                descriptor.public_generation(),
                state.last_public_generation,
            ) {
                return error;
            }
            let existing_host = state
                .host
                .as_ref()
                .map(crate::session_engine::SessionEngine::id);
            if let Err(error) = validate_platform_context_state(
                existing_host,
                state.platform_identity,
                state.platform_context.is_some(),
            ) {
                return error;
            }
            // Reserve this cold transition, then release SessionControl before
            // native construction, Host startup, and any callback-capable work.
            state.surface_transition = SurfaceTransition::Attaching;
            state.surface_transition_generation = Some(public_generation.get());
            debug_assert!(state.pending_surface_loss.is_none());
            state.callbacks_frozen = true;
            (
                existing_host,
                state.platform_identity,
                state.platform_context.clone(),
                state.callbacks,
                state.window_state.clone(),
            )
        };

        let (surface, graphics_platform, target, candidate_platform_context) =
            match build_target(descriptor, existing_platform_context.as_ref()) {
                Ok(built) => built,
                Err(error) => {
                    rollback_surface_transition(&session);
                    return error;
                }
            };
        let candidate_platform_identity = graphics_platform.platform_identity();
        if let Err(error) = validate_platform_identity(
            existing_host,
            existing_platform_identity,
            candidate_platform_identity,
        ) {
            rollback_surface_transition(&session);
            return error;
        }

        let (host, resource, notifier, new_ingress, window_state, mut new_host) =
            match existing_host {
                Some(host) => {
                    let Some(window_state) = existing_window_state else {
                        tracing::error!(
                            "migo_session_attach_surface: existing Host has no window state"
                        );
                        rollback_surface_transition(&session);
                        return MIGO_ERROR_INTERNAL;
                    };
                    let Some(ingress) = session.ingress.get() else {
                        tracing::error!(
                            "migo_session_attach_surface: existing Host has no direct ingress"
                        );
                        rollback_surface_transition(&session);
                        return MIGO_ERROR_INTERNAL;
                    };
                    if ingress.host_id() != host {
                        tracing::error!(
                            "migo_session_attach_surface: Session ingress Host {} does not match {}",
                            ingress.host_id(),
                            host,
                        );
                        rollback_surface_transition(&session);
                        return MIGO_ERROR_INTERNAL;
                    }
                    let lease = match lease_surface_tracked(host, surface, public_generation) {
                        Ok(lease) => lease,
                        Err(error) => {
                            tracing::error!("migo_session_attach_surface: lease failed: {error}");
                            rollback_surface_transition(&session);
                            return MIGO_ERROR_INTERNAL;
                        }
                    };
                    let resource = lease.resource_lease();
                    // Publish before enqueue so a render-complete resize event can
                    // never race ahead of the window metrics it announces.
                    let previous_metrics = window_state.replace(window_metrics);
                    if let Err(error) = send_critical_command_to_host(
                        host,
                        HostCommand::UpdateSurface {
                            lease,
                            pixel_ratio: Some(pixel_ratio),
                        },
                    ) {
                        window_state.replace(previous_metrics);
                        // A token was issued but no Host owner accepted it. Retire
                        // it immediately so a failed attach cannot leave a hidden
                        // live generation behind.
                        let _ = retire_surface(host);
                        tracing::error!("migo_session_attach_surface: send failed: {error}");
                        rollback_surface_transition(&session);
                        return MIGO_ERROR_INTERNAL;
                    }
                    (host, resource, None, None, window_state, None)
                }
                None => {
                    debug_assert!(existing_window_state.is_none());
                    let window_state = Arc::new(HostWindowState::new(window_metrics));
                    let notifier = configured_callbacks.map(|callbacks| {
                        Arc::new(callbacks::Notifier::new(
                            callbacks,
                            Arc::downgrade(&session),
                        ))
                    });
                    let host_kit: Arc<dyn PlatformServices> = Arc::new(host_kit::CapiHostKit::new(
                        notifier.clone(),
                        Arc::downgrade(&session),
                        Arc::clone(&window_state),
                    ));
                    let options = session
                        .engine
                        .session_init_options(configuration.scale_factor());
                    // The identity an external producer's packets must carry.
                    // An external-frame product refuses to start without one:
                    // a session with no identity would accept a packet from
                    // any process that guessed zero.
                    #[cfg(feature = "external-frames")]
                    let Some(launch_nonce) = session.launch_nonce else {
                        tracing::error!(
                            "migo_session_attach_surface: an external-frame session needs a \
                             launch_nonce in its MigoSessionConfig"
                        );
                        return MIGO_ERROR_INVALID_STATE;
                    };
                    #[cfg(not(feature = "external-frames"))]
                    let launch_nonce = session.launch_nonce.unwrap_or_default();

                    match crate::session_engine::spawn_tracked(
                        surface,
                        public_generation,
                        graphics_platform,
                        host_kit,
                        options,
                        launch_nonce,
                    ) {
                        Ok(started) => {
                            let host = started.engine.id();
                            // Handed over by the spawn. It used to be recovered
                            // here by asking the Host registry for the id that had
                            // just been started, which meant racing this session's
                            // own teardown: a renderer that failed to initialise
                            // takes the session down, and the session removes its
                            // registry entry on the way out. On the iOS simulator
                            // with no ANGLE present, this call lost that race about
                            // two runs in three. What the host was told either way
                            // was a bare MIGO_ERROR_INTERNAL, or MIGO_OK, for one
                            // input -- and the renderer's own account of what went
                            // wrong reached nobody, which is now `RenderExit`'s job.
                            debug_assert_eq!(started.ingress.host_id(), host);
                            (
                                host,
                                started.resource,
                                notifier,
                                Some(started.ingress),
                                window_state,
                                Some(started.engine),
                            )
                        }
                        Err(error) => {
                            tracing::error!("migo_session_attach_surface: spawn failed: {error:?}");
                            // Deliberately not reported here.
                            //
                            // `spawn_tracked` inherits `spawn_session_thread`'s
                            // contract: every startup `Err` it returns has
                            // already reached the host through `on_error`
                            // exactly once, carrying the failing thread's own
                            // account of why it stopped.
                            //
                            // A report here used to sit in this branch, and it
                            // double-notified for the common case. The two
                            // causes -- a thread that never started, and a
                            // thread that started and then failed -- are
                            // distinguishable at this level only by matching on
                            // the error message, and the one this branch can
                            // describe is always the vaguer of the two. So the
                            // report lives where the silence actually was:
                            // beside the spawn that failed.
                            rollback_surface_transition(&session);
                            return MIGO_ERROR_INTERNAL;
                        }
                    }
                }
            };

        let attachment = Box::new(MigoSurfaceAttachment {
            session: session_ptr,
            public_generation,
            resource,
            target,
            width_pixels: configuration.width_pixels(),
            height_pixels: configuration.height_pixels(),
            pixel_ratio,
            window_state: Arc::clone(&window_state),
            lost: false,
        });
        let attachment_ptr = NonNull::from(attachment.as_ref()).as_ptr();

        if let Some(ingress) = new_ingress
            && session.ingress.set(ingress).is_err()
        {
            tracing::error!("migo_session_attach_surface: direct ingress was installed twice");
            let _ = retire_surface(host);
            if let Some(mut owner) = new_host.take()
                && let Err(error) = owner.shutdown_and_join()
            {
                tracing::error!("migo_session_attach_surface: rollback join failed: {error}");
            }
            rollback_surface_transition(&session);
            return MIGO_ERROR_INTERNAL;
        }

        let (visibility, focused, deferred_loss) = {
            let Ok(mut state) = session.state.lock() else {
                let _ = retire_surface(host);
                if let Some(mut owner) = new_host.take()
                    && let Err(error) = owner.shutdown_and_join()
                {
                    tracing::error!("migo_session_attach_surface: rollback join failed: {error}");
                }
                // Release the reservation like every other pre-commit failure,
                // including this one's own sibling a few lines up, so the
                // recorded state still describes what is installed. It does not
                // resurrect the Session: the poison flag outlives this call and
                // every later entry point takes the guard with a plain `lock()`.
                // See `rollback_surface_transition` for why clearing the poison
                // would be a guess rather than a recovery.
                rollback_surface_transition(&session);
                return MIGO_ERROR_INTERNAL;
            };
            debug_assert_eq!(state.surface_transition, SurfaceTransition::Attaching);
            debug_assert!(state.active_attachment.is_none());
            if existing_host.is_none() {
                state.host = new_host.take();
                debug_assert!(state.host.is_some());
                state.platform_identity = Some(candidate_platform_identity);
                state.platform_context = Some(candidate_platform_context);
                state.notifier = notifier;
                state.window_state = Some(window_state);
            } else {
                debug_assert!(state.platform_context.is_some());
                debug_assert!(
                    state
                        .window_state
                        .as_ref()
                        .is_some_and(|installed| Arc::ptr_eq(installed, &window_state))
                );
            }
            state.active_attachment = Some(attachment);
            state.last_public_generation = descriptor.public_generation();
            let deferred_loss = consume_pending_surface_loss(&session, &mut state);
            if deferred_loss.is_none() {
                // Publish the live data-plane generation before opening the
                // control state to loss/detach observers.
                session.active_surface_generation.store(
                    public_generation.get(),
                    std::sync::atomic::Ordering::Release,
                );
            }
            // The output handle is part of the same commit boundary: once the
            // transition becomes Idle, an inline loss callback may re-enter
            // detach and therefore must already be able to find this pointer.
            *out_attachment = attachment_ptr;
            state.surface_transition_generation = None;
            state.surface_transition = SurfaceTransition::Idle;
            (state.visibility, state.focused, deferred_loss)
        };

        if let Some(loss) = deferred_loss {
            loss.notify();
        }

        // Deliver any visibility the host set before the surface existed,
        // which is the normal Android ordering.
        if let Some(visible) = visibility {
            let command = if visible {
                HostCommand::OnShow { options_json: None }
            } else {
                HostCommand::OnHide
            };
            if let Err(error) = migo_core::send_critical_command_to_host(host, command) {
                tracing::error!("attach: deferred visibility failed: {error}");
            }
        }
        if let Some(focused) = focused {
            if let Err(error) = migo_core::send_critical_command_to_host(
                host,
                HostCommand::OnFocusChanged { focused },
            ) {
                tracing::error!("attach: deferred focus failed: {error}");
            }
        }
        MIGO_OK
    })
}

/// Report a resize or a presentation-parameter change.
///
/// # Safety
/// `attachment` must come from [`migo_session_attach_surface`] and still be
/// live; `metrics` must point at a `MigoSurfaceMetrics` whose `struct_size`
/// matches the caller's ABI version.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_surface_update(
    attachment: *mut MigoSurfaceAttachment,
    metrics: *const MigoSurfaceMetrics,
) -> MigoResult {
    guard("migo_surface_update", || {
        let Some(attachment_ref) = (unsafe { attachment.as_ref() }) else {
            return MIGO_ERROR_INVALID_ARGUMENT;
        };
        let metrics = match unsafe { MigoSurfaceMetrics::parse(metrics) } {
            Ok(metrics) => metrics,
            Err(error) => return error,
        };
        let configuration = metrics.configuration();

        let session = match unsafe { pin_session(attachment_ref.session.as_ptr()) } {
            Ok(session) => session,
            Err(error) => return error,
        };

        let (host, target, resource, window_state) = {
            let Ok(mut state) = session.state.lock() else {
                return MIGO_ERROR_INTERNAL;
            };
            if state.surface_transition != SurfaceTransition::Idle {
                return MIGO_ERROR_INVALID_STATE;
            }
            let Some(active) = state.active_attachment.as_ref() else {
                return MIGO_ERROR_STALE_SURFACE;
            };
            if !std::ptr::eq(active.as_ref(), attachment_ref) {
                return MIGO_ERROR_STALE_SURFACE;
            }
            if active.lost {
                return MIGO_ERROR_STALE_SURFACE;
            }
            if let Err(error) = validate_update_generation(
                metrics.public_generation(),
                active.public_generation.get(),
            ) {
                return error;
            }
            let Some(host) = state
                .host
                .as_ref()
                .map(crate::session_engine::SessionEngine::id)
            else {
                return MIGO_ERROR_INVALID_STATE;
            };
            let transition_generation = active.public_generation();
            let snapshot = (
                host,
                active.target.clone(),
                active.resource.clone(),
                Arc::clone(&active.window_state),
            );
            state.surface_transition = SurfaceTransition::Updating;
            state.surface_transition_generation = Some(transition_generation);
            debug_assert!(state.pending_surface_loss.is_none());
            snapshot
        };

        let surface = match rebuild_surface(
            target,
            configuration.width_pixels(),
            configuration.height_pixels(),
        ) {
            Ok(surface) => surface,
            Err(error) => {
                rollback_surface_transition(&session);
                return error;
            }
        };
        let lease = match lease_surface_with_resource(host, surface, resource) {
            Ok(lease) => lease,
            Err(error) => {
                tracing::error!("migo_surface_update: lease failed: {error}");
                rollback_surface_transition(&session);
                return MIGO_ERROR_INTERNAL;
            }
        };
        // Critical, like Android's path: a dropped resize strands the app on a
        // stale-sized frame with no further callback to recover from.
        let pixel_ratio =
            PixelRatio::new(configuration.scale_factor()).expect("validated Surface scale factor");
        let previous_metrics = window_state.replace(HostWindowMetrics::new(
            configuration.width_pixels(),
            configuration.height_pixels(),
            pixel_ratio,
        ));
        if let Err(error) = send_critical_command_to_host(
            host,
            HostCommand::UpdateSurface {
                lease,
                pixel_ratio: Some(pixel_ratio),
            },
        ) {
            window_state.replace(previous_metrics);
            tracing::error!("migo_surface_update: send failed: {error}");
            rollback_surface_transition(&session);
            return MIGO_ERROR_INTERNAL;
        }

        let deferred_loss = {
            // Enqueue committed the update. Recover poison so the transition
            // and any loss racing it cannot remain permanently stranded.
            let mut state = session
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let active = state
                .active_attachment
                .as_mut()
                .expect("the reserved update attachment cannot disappear");
            assert!(std::ptr::eq(active.as_ref(), attachment_ref));
            active.width_pixels = configuration.width_pixels();
            active.height_pixels = configuration.height_pixels();
            active.pixel_ratio = pixel_ratio;
            let deferred_loss = consume_pending_surface_loss(&session, &mut state);
            state.surface_transition_generation = None;
            state.surface_transition = SurfaceTransition::Idle;
            deferred_loss
        };
        if let Some(loss) = deferred_loss {
            loss.notify();
        }
        MIGO_OK
    })
}

/// Begin asynchronous retirement of one unique attachment.
///
/// Successful retirement consumes the borrowed attachment identity and returns
/// a level-triggered observer. The host must keep the native resource and its
/// event loop alive until that observer reports RELEASED.
///
/// # Safety
/// `attachment` must be the active pointer returned by
/// [`migo_session_attach_surface`]. `out_release` must be writable for one
/// pointer. On `MIGO_OK`, `attachment` is invalid and `out_release` owns the
/// returned handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_surface_begin_detach(
    attachment: *mut MigoSurfaceAttachment,
    out_release: *mut *mut MigoSurfaceRelease,
) -> MigoResult {
    guard("migo_surface_begin_detach", || {
        let Some(out_release) = (unsafe { out_release.as_mut() }) else {
            return MIGO_ERROR_INVALID_ARGUMENT;
        };
        *out_release = std::ptr::null_mut();
        let Some(attachment_ref) = (unsafe { attachment.as_ref() }) else {
            return MIGO_ERROR_INVALID_ARGUMENT;
        };
        let session = match unsafe { pin_session(attachment_ref.session.as_ptr()) } {
            Ok(session) => session,
            Err(error) => return error,
        };

        // Allocate the public handle before retirement. Once the generation is
        // retired the ABI call is committed and must not surface a retryable
        // error whose retry would target a different state.
        let release_storage = Box::<MaybeUninit<MigoSurfaceRelease>>::new(MaybeUninit::uninit());

        let (host, internal_generation, prepared) = {
            let Ok(mut state) = session.state.lock() else {
                return MIGO_ERROR_INTERNAL;
            };
            if state.surface_transition != SurfaceTransition::Idle {
                return MIGO_ERROR_INVALID_STATE;
            }
            let Some(active) = state.active_attachment.as_ref() else {
                return MIGO_ERROR_STALE_SURFACE;
            };
            if !std::ptr::eq(active.as_ref(), attachment_ref) {
                return MIGO_ERROR_STALE_SURFACE;
            }
            let Some(host) = state
                .host
                .as_ref()
                .map(crate::session_engine::SessionEngine::id)
            else {
                return MIGO_ERROR_INVALID_STATE;
            };
            let notification = release_notification(state.notifier.as_ref());
            let prepared = match active
                .resource
                .prepare_release(Arc::clone(&session.pending_surface_releases), notification)
            {
                Ok(prepared) => prepared,
                Err(error) => {
                    tracing::error!("migo_surface_begin_detach: prepare failed: {error}");
                    return MIGO_ERROR_INVALID_STATE;
                }
            };
            let internal_generation = active.resource.internal_generation();
            let transition_generation = active.public_generation();
            state.surface_transition = SurfaceTransition::Detaching;
            state.surface_transition_generation = Some(transition_generation);
            debug_assert!(state.pending_surface_loss.is_none());
            (host, internal_generation, prepared)
        };

        // Close data-plane admission before the irreversible presentation
        // boundary. A caller that already acquired the old generation belongs
        // to the pre-detach stream; new callers fail immediately.
        session
            .active_surface_generation
            .store(0, std::sync::atomic::Ordering::Release);

        // This is the irreversible presentation boundary. A missing registry
        // means the Host has already exited and released (or is releasing) all
        // render ownership, which is also terminal and safe to commit.
        match retire_surface(host) {
            Ok(Some(retired)) if retired != internal_generation => {
                tracing::error!(
                    "migo_surface_begin_detach: retired internal generation {} but attachment owns {}",
                    retired.get(),
                    internal_generation.get(),
                );
            }
            Ok(_) => {}
            Err(error) => {
                tracing::debug!(
                    "migo_surface_begin_detach: Host already unavailable during retire: {error}"
                );
            }
        }

        let (owned_attachment, observer) = {
            // Retirement already committed. Recovering the contained state is
            // the only ownership-safe response to an otherwise impossible
            // poison here; returning an error would invite a stale retry and
            // strand the pending-release registration.
            let mut state = session
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let owned_attachment = state
                .active_attachment
                .take()
                .expect("the reserved attachment cannot disappear during detach");
            debug_assert!(std::ptr::eq(owned_attachment.as_ref(), attachment_ref));
            state.pending_surface_loss = None;
            state.surface_transition_generation = None;
            state.surface_transition = SurfaceTransition::Idle;
            let observer = prepared.commit();
            (owned_attachment, observer)
        };

        // Initialize and publish the preallocated handle before dropping the
        // attachment's resource lease. That final drop may complete release and
        // synchronously dispatch the optional wakeup callback.
        let release_storage = Box::into_raw(release_storage);
        unsafe {
            (*release_storage).write(MigoSurfaceRelease { observer });
        }
        *out_release = release_storage.cast::<MigoSurfaceRelease>();

        let _ = send_critical_command_to_host(
            host,
            HostCommand::SurfaceDestroyed {
                generation: internal_generation,
            },
        );
        drop(owned_attachment);
        MIGO_OK
    })
}

/// Query authoritative native-resource release state without blocking.
///
/// # Safety
/// `release` must be a live release handle and `out_status` a writable
/// versioned output record.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_surface_release_query(
    release: *const MigoSurfaceRelease,
    out_status: *mut MigoSurfaceReleaseStatus,
) -> MigoResult {
    guard("migo_surface_release_query", || {
        let Some(release) = (unsafe { release.as_ref() }) else {
            return MIGO_ERROR_INVALID_ARGUMENT;
        };
        let state = match release.observer.phase() {
            SurfaceReleasePhase::Pending => migo_capi_abi::surface::MIGO_SURFACE_RELEASE_PENDING,
            SurfaceReleasePhase::Released => migo_capi_abi::surface::MIGO_SURFACE_RELEASE_RELEASED,
        };
        unsafe {
            migo_capi_abi::surface::write_surface_release_status(
                out_status,
                release.observer.public_generation().get(),
                state,
            )
        }
    })
}

/// Destroy a completed release observer.
///
/// # Safety
/// `release` must be a unique live handle. `MIGO_OK` consumes it; a pending
/// result leaves ownership with the caller.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_surface_release_destroy(
    release: *mut MigoSurfaceRelease,
) -> MigoResult {
    guard("migo_surface_release_destroy", || {
        let Some(release_ref) = (unsafe { release.as_ref() }) else {
            return MIGO_ERROR_INVALID_ARGUMENT;
        };
        if release_ref.observer.phase() != SurfaceReleasePhase::Released {
            return MIGO_ERROR_INVALID_STATE;
        }
        drop(unsafe { Box::from_raw(release) });
        MIGO_OK
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        migo_session_create, migo_session_destroy,
        platform::{test_platform_context, test_platform_target},
        test_support::{session_config, with_engine, with_session},
    };
    use graphics::egl_platform::{GraphicsBackendId, PlatformIdentity};
    use migo_capi_abi::{MIGO_ABI_VERSION_CURRENT, MIGO_ERROR_UNSUPPORTED_ABI, VersionedHeader};
    use shared::surface::{Surface, SurfaceControl, SurfaceLease, SurfaceResourceLease};

    #[derive(Debug)]
    struct TestSurface;
    struct TestBackend;
    struct TestOtherBackend;
    struct TestDomain;
    struct TestOtherDomain;

    impl Surface for TestSurface {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn size(&self) -> (u32, u32) {
            (640, 480)
        }
    }

    #[test]
    fn platform_identity_validation_fails_closed_before_reattach() {
        let backend = GraphicsBackendId::of::<TestBackend>();
        let original = PlatformIdentity::new::<TestDomain>(backend, 0x1000);

        assert_eq!(validate_platform_identity(None, None, original), Ok(()));
        assert_eq!(
            validate_platform_identity(Some(7), Some(original), original),
            Ok(())
        );
        assert_eq!(
            validate_platform_identity(
                Some(7),
                Some(original),
                PlatformIdentity::new::<TestDomain>(backend, 0x2000),
            ),
            Err(MIGO_ERROR_INVALID_STATE)
        );
        assert_eq!(
            validate_platform_identity(
                Some(7),
                Some(original),
                PlatformIdentity::new::<TestOtherDomain>(backend, 0x1000),
            ),
            Err(MIGO_ERROR_INVALID_STATE)
        );
        assert_eq!(
            validate_platform_identity(
                Some(7),
                Some(original),
                PlatformIdentity::new::<TestOtherBackend>(
                    GraphicsBackendId::of::<TestOtherBackend>(),
                    0x1000,
                ),
            ),
            Err(MIGO_ERROR_INVALID_STATE)
        );
        assert_eq!(
            validate_platform_identity(Some(7), None, original),
            Err(MIGO_ERROR_INTERNAL)
        );
        assert_eq!(
            validate_platform_identity(None, Some(original), original),
            Err(MIGO_ERROR_INTERNAL)
        );
    }

    #[test]
    fn platform_context_state_is_all_absent_or_all_committed() {
        let identity =
            PlatformIdentity::new::<TestDomain>(GraphicsBackendId::of::<TestBackend>(), 0x1000);

        assert_eq!(validate_platform_context_state(None, None, false), Ok(()));
        assert_eq!(
            validate_platform_context_state(Some(7), Some(identity), true),
            Ok(())
        );

        for inconsistent in [
            (Some(7), Some(identity), false),
            (Some(7), None, true),
            (Some(7), None, false),
            (None, Some(identity), true),
            (None, Some(identity), false),
            (None, None, true),
        ] {
            assert_eq!(
                validate_platform_context_state(inconsistent.0, inconsistent.1, inconsistent.2),
                Err(MIGO_ERROR_INTERNAL)
            );
        }
    }

    fn metrics(width: u32, height: u32) -> MigoSurfaceMetrics {
        MigoSurfaceMetrics {
            header: VersionedHeader {
                struct_size: size_of::<MigoSurfaceMetrics>() as u32,
                abi_version: MIGO_ABI_VERSION_CURRENT,
            },
            generation: 1,
            width_pixels: width,
            height_pixels: height,
            scale_factor: 1.0,
            color_space: 0,
            alpha_mode: 0,
            preferred_presentation_mode: 0,
            flags: 0,
            reserved0: 0,
        }
    }

    /// An attachment that is structurally valid but whose session has no host.
    ///
    /// Built directly rather than through attach so these tests need no X11
    /// display: every path they exercise rejects the call before it would reach
    /// the engine. The session behind it is real, so the state lock and the
    /// `attached` check are the production ones.
    fn detached_attachment(session: *mut MigoSession) -> MigoSurfaceAttachment {
        let control = SurfaceControl::new();
        let token = control.attach_or_update().expect("test generation");
        let surface: shared::surface::SurfaceRef = Arc::new(TestSurface);
        let public_generation = PublicSurfaceGeneration::new(1).expect("non-zero");
        let lease = SurfaceLease::new_tracked(surface, token, public_generation);
        MigoSurfaceAttachment {
            session: NonNull::new(session).expect("session"),
            public_generation,
            resource: lease.resource_lease(),
            target: test_platform_target(),
            width_pixels: 640,
            height_pixels: 480,
            pixel_ratio: PixelRatio::new(1.0).expect("valid test pixel ratio"),
            window_state: Arc::new(HostWindowState::new(HostWindowMetrics::new(
                640,
                480,
                PixelRatio::new(1.0).expect("valid test pixel ratio"),
            ))),
            lost: false,
        }
    }

    fn install_active_attachment(
        session: *mut MigoSession,
        retain_extra_lease: bool,
    ) -> (*mut MigoSurfaceAttachment, Option<SurfaceResourceLease>) {
        let attachment = Box::new(detached_attachment(session));
        let extra = retain_extra_lease.then(|| attachment.resource.clone());
        let pointer = NonNull::from(attachment.as_ref()).as_ptr();
        let session = unsafe { &*session };
        let mut state = session.state.lock().expect("SessionControl");
        let join = thread::Builder::new()
            .name("Migo-Main-capi-surface-test".to_owned())
            .spawn(|| {})
            .expect("spawn inert test Host");
        state.host = Some(crate::session_engine::engine_for_test(i32::MAX, join));
        state.platform_identity = Some(PlatformIdentity::new::<TestDomain>(
            GraphicsBackendId::of::<TestBackend>(),
            0,
        ));
        state.platform_context = Some(test_platform_context());
        state.last_public_generation = 1;
        state.active_attachment = Some(attachment);
        session
            .active_surface_generation
            .store(1, std::sync::atomic::Ordering::Release);
        (pointer, extra)
    }

    fn release_status(release: *const MigoSurfaceRelease) -> MigoSurfaceReleaseStatus {
        let mut status = MigoSurfaceReleaseStatus {
            header: VersionedHeader {
                struct_size: size_of::<MigoSurfaceReleaseStatus>() as u32,
                abi_version: MIGO_ABI_VERSION_CURRENT,
            },
            generation: 0,
            state: u32::MAX,
            reserved0: u32::MAX,
        };
        assert_eq!(
            unsafe { migo_surface_release_query(release, &mut status) },
            MIGO_OK,
        );
        status
    }

    #[test]
    fn unexpected_loss_closes_input_once_and_leaves_attachment_for_release() {
        crate::test_support::with_session("surface-loss-state", |session| {
            let (attachment, _extra) = install_active_attachment(session, false);
            let session_ref = unsafe { &*session };

            assert!(session_ref.mark_surface_lost(1, SurfaceLossReason::PlatformError));
            assert_eq!(
                session_ref
                    .active_surface_generation
                    .load(std::sync::atomic::Ordering::Acquire),
                0
            );
            assert!(!session_ref.mark_surface_lost(1, SurfaceLossReason::PlatformError));
            let state = session_ref.state.lock().expect("SessionControl");
            assert!(
                state
                    .active_attachment
                    .as_ref()
                    .is_some_and(|active| active.lost)
            );
            drop(state);

            let mut release = std::ptr::null_mut();
            assert_eq!(
                unsafe { migo_surface_begin_detach(attachment, &mut release) },
                MIGO_OK
            );
            assert!(!release.is_null());
            assert_eq!(unsafe { migo_surface_release_destroy(release) }, MIGO_OK);
        });
    }

    #[test]
    fn loss_during_update_is_deferred_until_the_control_transition_commits() {
        crate::test_support::with_session("surface-loss-update-race", |session| {
            let (attachment, _extra) = install_active_attachment(session, false);
            let session_ref = unsafe { &*session };
            {
                let mut state = session_ref.state.lock().expect("SessionControl");
                state.surface_transition = SurfaceTransition::Updating;
                state.surface_transition_generation = Some(1);
            }

            assert!(!session_ref.mark_surface_lost(1, SurfaceLossReason::PlatformError));
            assert_eq!(
                session_ref
                    .active_surface_generation
                    .load(std::sync::atomic::Ordering::Acquire),
                1,
                "the data-plane boundary moves atomically with transition completion",
            );

            rollback_surface_transition(session_ref);
            assert_eq!(
                session_ref
                    .active_surface_generation
                    .load(std::sync::atomic::Ordering::Acquire),
                0,
            );
            let state = session_ref.state.lock().expect("SessionControl");
            assert_eq!(state.surface_transition, SurfaceTransition::Idle);
            assert!(state.surface_transition_generation.is_none());
            assert!(
                state
                    .active_attachment
                    .as_ref()
                    .is_some_and(|active| active.lost),
            );
            assert!(state.pending_surface_loss.is_none());
            drop(state);

            // Retire it before the Session goes away. A Session refuses to be
            // destroyed while an attachment is live, so leaving one installed
            // would make the harness's teardown fail for a reason that has
            // nothing to do with what this test is about -- and losing the
            // Surface does not detach it, which is precisely the distinction
            // the assertions above are pinning down.
            let mut release = std::ptr::null_mut();
            assert_eq!(
                unsafe { migo_surface_begin_detach(attachment, &mut release) },
                MIGO_OK,
            );
            assert_eq!(unsafe { migo_surface_release_destroy(release) }, MIGO_OK);
        });
    }

    #[test]
    fn loss_for_the_generation_being_attached_is_recorded_before_installation() {
        crate::test_support::with_session("surface-loss-attach-race", |session| {
            let session_ref = unsafe { &*session };
            {
                let mut state = session_ref.state.lock().expect("SessionControl");
                state.surface_transition = SurfaceTransition::Attaching;
                state.surface_transition_generation = Some(7);
            }

            assert!(!session_ref.mark_surface_lost(7, SurfaceLossReason::PlatformError));
            let state = session_ref.state.lock().expect("SessionControl");
            assert_eq!(
                state.pending_surface_loss.map(|loss| loss.generation),
                Some(7),
            );
            drop(state);

            // A synchronously failed attach has no public attachment to lose;
            // rollback consumes the deferred report instead of leaking it into
            // the next generation.
            rollback_surface_transition(session_ref);
            let state = session_ref.state.lock().expect("SessionControl");
            assert!(state.pending_surface_loss.is_none());
            assert!(state.surface_transition_generation.is_none());
        });
    }

    #[test]
    fn update_rejects_a_null_attachment() {
        let m = metrics(800, 600);
        assert_eq!(
            unsafe { migo_surface_update(std::ptr::null_mut(), &m) },
            MIGO_ERROR_INVALID_ARGUMENT
        );
    }

    #[test]
    fn update_rejects_null_metrics() {
        with_session("update-null-metrics", |session| {
            let mut attachment = detached_attachment(session);
            assert_eq!(
                unsafe { migo_surface_update(&mut attachment, std::ptr::null()) },
                MIGO_ERROR_INVALID_ARGUMENT
            );
        });
    }

    #[test]
    fn update_rejects_a_struct_size_mismatch() {
        with_session("update-size-mismatch", |session| {
            let mut attachment = detached_attachment(session);
            let mut m = metrics(800, 600);
            m.header.struct_size = 8;
            assert_eq!(
                unsafe { migo_surface_update(&mut attachment, &m) },
                MIGO_ERROR_INVALID_ARGUMENT
            );
        });
    }

    #[test]
    fn update_rejects_an_unknown_abi_version() {
        with_session("update-abi-version", |session| {
            let mut attachment = detached_attachment(session);
            let mut m = metrics(800, 600);
            m.header.abi_version = 99;
            assert_eq!(
                unsafe { migo_surface_update(&mut attachment, &m) },
                MIGO_ERROR_UNSUPPORTED_ABI
            );
        });
    }

    #[test]
    fn update_rejects_a_zero_sized_surface() {
        with_session("update-zero-size", |session| {
            let mut attachment = detached_attachment(session);
            assert_eq!(
                unsafe { migo_surface_update(&mut attachment, &metrics(0, 600)) },
                MIGO_ERROR_INVALID_ARGUMENT
            );
            assert_eq!(
                unsafe { migo_surface_update(&mut attachment, &metrics(800, 0)) },
                MIGO_ERROR_INVALID_ARGUMENT
            );
        });
    }

    #[test]
    fn update_on_a_detached_session_reports_invalid_state() {
        // Nothing is attached, so there is no surface to resize. Reporting
        // success would tell the host its resize took effect when it did not.
        with_session("update-detached", |session| {
            let mut attachment = detached_attachment(session);
            assert_eq!(
                unsafe { migo_surface_update(&mut attachment, &metrics(800, 600)) },
                MIGO_ERROR_STALE_SURFACE
            );
        });
    }

    #[test]
    fn begin_detach_is_irreversible_and_release_remains_queryable_after_session_destroy() {
        with_engine("release-after-session", |engine| {
            let mut session = std::ptr::null_mut();
            assert_eq!(
                unsafe { migo_session_create(engine, &session_config(), &mut session) },
                MIGO_OK,
            );
            let (attachment, _extra) = install_active_attachment(session, false);
            let mut release = std::ptr::null_mut();

            assert_eq!(
                unsafe { migo_surface_begin_detach(attachment, &mut release) },
                MIGO_OK,
            );
            assert!(!release.is_null());
            let status = release_status(release);
            assert_eq!(status.generation, 1);
            assert_eq!(
                status.state,
                migo_capi_abi::surface::MIGO_SURFACE_RELEASE_RELEASED,
            );

            assert_eq!(unsafe { migo_session_destroy(session) }, MIGO_OK);
            assert_eq!(
                release_status(release).state,
                migo_capi_abi::surface::MIGO_SURFACE_RELEASE_RELEASED,
            );
            assert_eq!(unsafe { migo_surface_release_destroy(release) }, MIGO_OK);
        });
    }

    #[test]
    fn a_live_attachment_blocks_session_destroy_rather_than_being_consumed_by_it() {
        with_engine("destroy-with-live-attachment", |engine| {
            let mut session = std::ptr::null_mut();
            assert_eq!(
                unsafe { migo_session_create(engine, &session_config(), &mut session) },
                MIGO_OK,
            );
            let (attachment, _extra) = install_active_attachment(session, false);

            // Refusing is the contract. Consuming the attachment here instead
            // would tear the Session down while the GPU may still reference its
            // Surface, and would silently invalidate a pointer the caller still
            // believes it owns. A refusal leaves everything retryable.
            assert_eq!(
                unsafe { migo_session_destroy(session) },
                MIGO_ERROR_INVALID_STATE,
            );

            let mut release = std::ptr::null_mut();
            assert_eq!(
                unsafe { migo_surface_begin_detach(attachment, &mut release) },
                MIGO_OK,
            );
            assert_eq!(
                release_status(release).state,
                migo_capi_abi::surface::MIGO_SURFACE_RELEASE_RELEASED,
            );

            // The Session was never damaged by the refusal above.
            assert_eq!(unsafe { migo_session_destroy(session) }, MIGO_OK);
            assert_eq!(unsafe { migo_surface_release_destroy(release) }, MIGO_OK);
        });
    }

    #[test]
    fn pending_release_blocks_both_release_and_session_destroy_until_last_lease_drops() {
        with_engine("release-pending", |engine| {
            let mut session = std::ptr::null_mut();
            assert_eq!(
                unsafe { migo_session_create(engine, &session_config(), &mut session) },
                MIGO_OK,
            );
            let (attachment, extra) = install_active_attachment(session, true);
            let mut release = std::ptr::null_mut();
            assert_eq!(
                unsafe { migo_surface_begin_detach(attachment, &mut release) },
                MIGO_OK,
            );
            assert_eq!(
                release_status(release).state,
                migo_capi_abi::surface::MIGO_SURFACE_RELEASE_PENDING,
            );
            assert_eq!(
                unsafe { migo_surface_release_destroy(release) },
                MIGO_ERROR_INVALID_STATE,
            );
            assert_eq!(
                unsafe { migo_session_destroy(session) },
                MIGO_ERROR_INVALID_STATE,
            );

            drop(extra);
            assert_eq!(
                release_status(release).state,
                migo_capi_abi::surface::MIGO_SURFACE_RELEASE_RELEASED,
            );
            assert_eq!(unsafe { migo_session_destroy(session) }, MIGO_OK);
            assert_eq!(unsafe { migo_surface_release_destroy(release) }, MIGO_OK);
        });
    }

    #[test]
    fn a_non_owned_attachment_pointer_cannot_mutate_or_retire_the_active_one() {
        with_session("attachment-identity", |session| {
            let (attachment, _extra) = install_active_attachment(session, false);
            let mut impostor = detached_attachment(session);
            let mut release = 1usize as *mut MigoSurfaceRelease;

            assert_eq!(
                unsafe { migo_surface_update(&mut impostor, &metrics(800, 600)) },
                MIGO_ERROR_STALE_SURFACE,
            );
            assert_eq!(
                unsafe { migo_surface_begin_detach(&mut impostor, &mut release) },
                MIGO_ERROR_STALE_SURFACE,
            );
            assert!(release.is_null(), "rejected detach must clear stale output");

            assert_eq!(
                unsafe { migo_surface_begin_detach(attachment, &mut release) },
                MIGO_OK,
            );
            assert_eq!(unsafe { migo_surface_release_destroy(release) }, MIGO_OK);
        });
    }

    // A null-attachment detach is covered by
    // `tests::null_handles_are_argument_errors_not_crashes`, which checks every
    // handle-taking entry point in one place.
}
