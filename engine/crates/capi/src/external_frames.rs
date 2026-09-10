//! The entry points a frame transport calls.
//!
//! Compiled only into the external-frame product. There is no placeholder in
//! the other products, and that is deliberate: an exported symbol that always
//! fails is what shipped a Windows SDK which loaded, resolved every entry
//! point, and could attach nothing. A host that links the wrong product gets an
//! undefined symbol at build time instead, which is the loud, early failure.
//!
//! `migo_session_load_content` on a session created for this lane returns
//! `MIGO_ERROR_INVALID_STATE`, because there is no JavaScript runtime here to
//! evaluate anything: the content's code runs in WebKit's WebContent process.

use migo_capi_abi::{
    MIGO_ERROR_INTERNAL, MIGO_ERROR_INVALID_ARGUMENT, MIGO_ERROR_INVALID_STATE, MIGO_OK,
    MigoResult,
    external_frames::{
        MIGO_FRAME_INGRESS_ACCEPTED, MIGO_FRAME_INGRESS_GENERATION_LOST,
        MIGO_FRAME_INGRESS_REJECTED, MIGO_FRAME_INGRESS_WOULD_BLOCK,
        MIGO_SYNC_ERROR_ALREADY_PENDING, MIGO_SYNC_ERROR_BAD_DEADLINE,
        MIGO_SYNC_ERROR_BAD_REPLY_RESERVATION, MIGO_SYNC_ERROR_LATE_REPLY,
        MIGO_SYNC_ERROR_REPLY_TOO_LARGE, MIGO_SYNC_ERROR_REQUEST_ID_MISMATCH,
        MIGO_SYNC_ERROR_SESSION_ENDED, MIGO_SYNC_ERROR_STALE_GENERATION, MIGO_SYNC_ERROR_TIMED_OUT,
        MIGO_SYNC_ERROR_UNSUPPORTED_OPERATION, MIGO_SYNC_STATE_CANCELLED, MIGO_SYNC_STATE_FAILED,
        MIGO_SYNC_STATE_FREE, MIGO_SYNC_STATE_PENDING, MIGO_SYNC_STATE_READY,
        MigoFrameIngressOutcome, MigoSyncOutcome, MigoSyncRequestDescriptor,
        write_frame_ingress_outcome, write_sync_outcome,
    },
};
use migo_core::IngressDecision;

use crate::{MigoSession, panic_barrier::guard, pin_session};

/// Offer one frame produced outside this process.
///
/// `bytes` is borrowed for the duration of this call only. An accepted packet
/// is copied once into a buffer the library owns before this returns, so the
/// caller may reuse or free its own storage immediately -- which is what lets
/// a Swift transport hand over a `Data`'s interior pointer without keeping it
/// alive.
///
/// # Safety
/// `session` must be a live session handle. `bytes` must be readable for
/// `byte_count` bytes, or null when `byte_count` is zero. `out_outcome` must
/// satisfy the versioned-output contract.
#[cfg(feature = "external-frames")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_submit_external_frame(
    session: *mut MigoSession,
    bytes: *const u8,
    byte_count: usize,
    out_outcome: *mut MigoFrameIngressOutcome,
) -> MigoResult {
    guard("migo_session_submit_external_frame", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        if bytes.is_null() || byte_count == 0 {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        // A packet larger than the address space is not a packet; refusing here
        // keeps the slice construction below total.
        if byte_count > isize::MAX as usize {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }

        // Validate the caller's output before accepting work. Otherwise a null
        // or incompatible output can report failure after the frame was queued
        // and its sequence committed, leaving the caller unable to retry it.
        // SAFETY: the versioned-output contract guarantees a readable header.
        if let Err(error) = unsafe {
            migo_capi_abi::validate_header(out_outcome.cast(), size_of::<MigoFrameIngressOutcome>())
        } {
            return error;
        }

        let Ok(state) = session.state.lock() else {
            return MIGO_ERROR_INTERNAL;
        };
        let Some(engine) = state.host.as_ref() else {
            // No surface has been attached, so there is no renderer to hand a
            // frame to. Not an error in the packet: the host called in the
            // wrong order.
            return MIGO_ERROR_INVALID_STATE;
        };

        // SAFETY: null and length were checked above; the contract requires the
        // range to be readable for the call, and nothing derived from it
        // outlives this function -- an accepted packet is copied.
        let packet = unsafe { std::slice::from_raw_parts(bytes, byte_count) };
        let outcome = engine.submit_frame(packet);
        drop(state);

        let decision = match outcome.decision {
            IngressDecision::Accepted => MIGO_FRAME_INGRESS_ACCEPTED,
            IngressDecision::WouldBlock => MIGO_FRAME_INGRESS_WOULD_BLOCK,
            IngressDecision::Rejected => MIGO_FRAME_INGRESS_REJECTED,
            IngressDecision::GenerationLost => MIGO_FRAME_INGRESS_GENERATION_LOST,
        };
        // SAFETY: forwarded from this function's output contract.
        unsafe {
            write_frame_ingress_outcome(
                out_outcome,
                decision,
                outcome.remaining_credits,
                outcome.accepted_sequence,
                outcome.wire_error_code,
            )
        }
    })
}

/// Ask for one frame.
///
/// The producer is blocked on the host's clock: it renders when told to, the
/// same way every other Migo platform's `requestAnimationFrame` is fed by host
/// vsync rather than by a browser. Returns `MIGO_ERROR_INVALID_STATE` when the
/// renderer is not up yet, which is the truthful answer for a session that
/// cannot produce a frame.
///
/// # Safety
/// `session` must be a live session handle.
#[cfg(feature = "external-frames")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_request_external_frame(
    session: *mut MigoSession,
) -> MigoResult {
    guard("migo_session_request_external_frame", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        let Ok(state) = session.state.lock() else {
            return MIGO_ERROR_INTERNAL;
        };
        let Some(engine) = state.host.as_ref() else {
            return MIGO_ERROR_INVALID_STATE;
        };
        if engine.clock().request_frame() {
            MIGO_OK
        } else {
            MIGO_ERROR_INVALID_STATE
        }
    })
}

/// Drain one pending WebGL error for a canvas, or report that there is none.
///
/// `gl.getError()` is a synchronous call the producer makes in another process.
/// The errors the decoder recorded wait here until it asks.
///
/// # Safety
/// `session` must be a live session handle. `out_code` must be writable.
#[cfg(feature = "external-frames")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_take_external_gl_error(
    session: *mut MigoSession,
    canvas_id: u32,
    out_code: *mut u32,
) -> MigoResult {
    guard("migo_session_take_external_gl_error", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        let Some(out_code) = (unsafe { out_code.as_mut() }) else {
            return MIGO_ERROR_INVALID_ARGUMENT;
        };
        let Ok(state) = session.state.lock() else {
            return MIGO_ERROR_INTERNAL;
        };
        let Some(engine) = state.host.as_ref() else {
            return MIGO_ERROR_INVALID_STATE;
        };
        // `NO_ERROR` is zero, which is what WebGL returns from an empty queue,
        // so an empty queue is not an error at this boundary either.
        *out_code = engine.drain_gl_error(canvas_id).unwrap_or(0);
        MIGO_OK
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_session;
    use migo_capi_abi::VersionedHeader;

    fn outcome() -> MigoFrameIngressOutcome {
        MigoFrameIngressOutcome {
            header: VersionedHeader {
                struct_size: size_of::<MigoFrameIngressOutcome>() as u32,
                abi_version: 1,
            },
            accepted_sequence: 0,
            decision: 0,
            remaining_credits: 0,
            wire_error_code: 0,
            reserved0: 0,
        }
    }

    /// A frame offered before a surface is attached has no renderer to reach.
    ///
    /// Reported as a call that could not be made rather than as a rejected
    /// packet: the bytes may be perfectly good, and telling the producer they
    /// were rejected would send it looking for a bug in its encoder.
    #[test]
    fn submitting_before_a_surface_is_attached_is_a_state_error() {
        with_session("external-submit-no-surface", |session| {
            let packet = [0u8; 96];
            let mut out = outcome();
            let result = unsafe {
                migo_session_submit_external_frame(session, packet.as_ptr(), packet.len(), &mut out)
            };
            assert_eq!(result, MIGO_ERROR_INVALID_STATE);
            assert_eq!(out.decision, 0, "nothing was decided about the packet");
        });
    }

    #[test]
    fn a_null_or_empty_packet_is_refused_before_anything_is_locked() {
        with_session("external-submit-null", |session| {
            let mut out = outcome();
            assert_eq!(
                unsafe {
                    migo_session_submit_external_frame(session, std::ptr::null(), 96, &mut out)
                },
                MIGO_ERROR_INVALID_ARGUMENT
            );
            let packet = [0u8; 96];
            assert_eq!(
                unsafe {
                    migo_session_submit_external_frame(session, packet.as_ptr(), 0, &mut out)
                },
                MIGO_ERROR_INVALID_ARGUMENT,
                "a zero-length packet is shorter than the header, not an empty frame"
            );
        });
    }

    #[test]
    fn output_is_validated_before_reaching_the_renderer() {
        with_session("external-submit-invalid-output", |session| {
            let packet = [0u8; 96];
            assert_eq!(
                unsafe {
                    migo_session_submit_external_frame(
                        session,
                        packet.as_ptr(),
                        packet.len(),
                        std::ptr::null_mut(),
                    )
                },
                MIGO_ERROR_INVALID_ARGUMENT
            );
            for (size, version, expected) in [
                (8, 1, MIGO_ERROR_INVALID_ARGUMENT),
                (
                    size_of::<MigoFrameIngressOutcome>() as u32,
                    99,
                    migo_capi_abi::MIGO_ERROR_UNSUPPORTED_ABI,
                ),
            ] {
                let mut out = outcome();
                out.header.struct_size = size;
                out.header.abi_version = version;
                let before = out;
                assert_eq!(
                    unsafe {
                        migo_session_submit_external_frame(
                            session,
                            packet.as_ptr(),
                            packet.len(),
                            &mut out,
                        )
                    },
                    expected
                );
                assert_eq!(out, before);
            }
        });
    }

    #[test]
    fn a_null_session_is_refused_by_every_entry_point() {
        let mut out = outcome();
        let packet = [0u8; 96];
        assert_eq!(
            unsafe {
                migo_session_submit_external_frame(
                    std::ptr::null_mut(),
                    packet.as_ptr(),
                    packet.len(),
                    &mut out,
                )
            },
            MIGO_ERROR_INVALID_ARGUMENT
        );
        assert_eq!(
            unsafe { migo_session_request_external_frame(std::ptr::null_mut()) },
            MIGO_ERROR_INVALID_ARGUMENT
        );
        let mut code = 0u32;
        assert_eq!(
            unsafe { migo_session_take_external_gl_error(std::ptr::null_mut(), 1, &mut code) },
            MIGO_ERROR_INVALID_ARGUMENT
        );
    }

    #[test]
    fn asking_for_a_frame_before_the_renderer_is_up_is_a_state_error() {
        with_session("external-request-frame", |session| {
            assert_eq!(
                unsafe { migo_session_request_external_frame(session) },
                MIGO_ERROR_INVALID_STATE
            );
        });
    }

    #[test]
    fn taking_an_error_needs_somewhere_to_put_it() {
        with_session("external-take-error", |session| {
            assert_eq!(
                unsafe { migo_session_take_external_gl_error(session, 1, std::ptr::null_mut()) },
                MIGO_ERROR_INVALID_ARGUMENT
            );
        });
    }
}

// ---------------------------------------------------------------------------
// The synchronous barrier
// ---------------------------------------------------------------------------

/// Turn a mailbox verdict into the number the producer reads.
///
/// A table rather than a cast, because these numbers are a public contract the
/// producer turns into exceptions its own code catches: renumbering the Rust
/// enum must not silently renumber the wire.
#[cfg(feature = "external-frames")]
fn sync_error_code(error: migo_core::SyncError) -> u32 {
    use migo_core::SyncError as E;
    match error {
        E::AlreadyPending => MIGO_SYNC_ERROR_ALREADY_PENDING,
        E::RequestIdMismatch => MIGO_SYNC_ERROR_REQUEST_ID_MISMATCH,
        E::StaleGeneration => MIGO_SYNC_ERROR_STALE_GENERATION,
        E::ReplyTooLarge => MIGO_SYNC_ERROR_REPLY_TOO_LARGE,
        E::TimedOut => MIGO_SYNC_ERROR_TIMED_OUT,
        E::SessionEnded => MIGO_SYNC_ERROR_SESSION_ENDED,
        E::UnsupportedOperation => MIGO_SYNC_ERROR_UNSUPPORTED_OPERATION,
        E::LateReply => MIGO_SYNC_ERROR_LATE_REPLY,
        E::BadDeadline => MIGO_SYNC_ERROR_BAD_DEADLINE,
        E::BadReplyReservation => MIGO_SYNC_ERROR_BAD_REPLY_RESERVATION,
    }
}

#[cfg(feature = "external-frames")]
fn sync_state_code(state: migo_core::SyncState) -> u32 {
    use migo_core::SyncState as S;
    match state {
        S::Free => MIGO_SYNC_STATE_FREE,
        S::Pending => MIGO_SYNC_STATE_PENDING,
        S::Ready => MIGO_SYNC_STATE_READY,
        S::Failed => MIGO_SYNC_STATE_FAILED,
        S::Cancelled => MIGO_SYNC_STATE_CANCELLED,
    }
}

/// Post one synchronous request and answer it.
///
/// `readPixels` cannot be answered where the producer runs: its return value
/// *is* the answer, and the pixels are here. The producer blocks, the transport
/// carries the request across, this answers it, and `migo_session_take_sync_reply`
/// carries the bytes back.
///
/// `now_nanos` is the caller's monotonic clock reading, the same clock
/// `deadline_nanos` is expressed on. The library does not read a clock of its
/// own for this, deliberately: two clocks that agree today are a defect waiting
/// for the platform where they do not, and only the host can read the clock its
/// producer blocked against.
///
/// `params` carries the operation's arguments, which are not in the descriptor
/// because the descriptor is a fixed rendezvous record and per-operation
/// arguments would size it by the largest operation anyone ever adds. For
/// `MIGO_SYNC_OP_READ_PIXELS` it is 32 bytes: canvas id, x, y, width, height,
/// format, type, reserved -- each a little-endian 32-bit word.
///
/// A request the host cannot answer still returns `MIGO_OK` and reports
/// `MIGO_SYNC_STATE_FAILED` with a reason, for the same reason
/// `migo_session_submit_external_frame` reports a rejection rather than failing:
/// the call did its job, and the verdict belongs in the outcome where the
/// producer can read it. A refusal that never got an id reports `request_id`
/// zero.
///
/// # Safety
/// `session` must be a live session handle. `request` must point at a
/// `MigoSyncRequestDescriptor` whose `struct_size` and `abi_version` this
/// library recognises. `params` must be readable for `param_bytes`, or null
/// when `param_bytes` is zero. `out_outcome` must satisfy the versioned-output
/// contract.
#[cfg(feature = "external-frames")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_post_sync_request(
    session: *mut MigoSession,
    request: *const MigoSyncRequestDescriptor,
    params: *const u8,
    param_bytes: usize,
    now_nanos: u64,
    out_outcome: *mut MigoSyncOutcome,
) -> MigoResult {
    guard("migo_session_post_sync_request", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        // SAFETY: the versioned-output contract guarantees a readable header.
        if let Err(error) = unsafe {
            migo_capi_abi::validate_header(out_outcome.cast(), size_of::<MigoSyncOutcome>())
        } {
            return error;
        }
        // SAFETY: the caller's descriptor begins with the same two words every
        // versioned record does.
        if let Err(error) = unsafe {
            migo_capi_abi::validate_header(request.cast(), size_of::<MigoSyncRequestDescriptor>())
        } {
            return error;
        }
        // SAFETY: validated above, so the full record is readable.
        let descriptor = unsafe { *request };

        if param_bytes > isize::MAX as usize {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        if params.is_null() && param_bytes != 0 {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        // SAFETY: null and length were checked; nothing derived from the slice
        // outlives this call -- the arguments are decoded into owned values.
        let params = if param_bytes == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(params, param_bytes) }
        };

        let Ok(state) = session.state.lock() else {
            return MIGO_ERROR_INTERNAL;
        };
        let Some(engine) = state.host.as_ref() else {
            return MIGO_ERROR_INVALID_STATE;
        };

        let posted = engine.post_sync_request(
            migo_core::SyncRequest {
                request_id: 0,
                runtime_generation: descriptor.runtime_generation,
                surface_generation: descriptor.surface_generation,
                resource_epoch: descriptor.resource_epoch,
                triggering_sequence: descriptor.triggering_sequence,
                operation: descriptor.operation,
                max_reply_bytes: descriptor.max_reply_bytes,
                deadline_nanos: descriptor.deadline_nanos,
            },
            params,
            now_nanos,
        );
        let snapshot = match posted {
            Ok(_) => engine.poll_sync(now_nanos),
            Err(error) => {
                drop(state);
                // SAFETY: forwarded from this function's output contract.
                return unsafe {
                    write_sync_outcome(
                        out_outcome,
                        0,
                        MIGO_SYNC_STATE_FAILED,
                        0,
                        sync_error_code(error),
                    )
                };
            }
        };
        drop(state);

        // SAFETY: forwarded from this function's output contract.
        unsafe { write_sync_snapshot(out_outcome, snapshot) }
    })
}

/// Where the outstanding request is.
///
/// Also what makes a passed deadline visible: nothing else runs while a request
/// is outstanding, so a producer whose deadline elapsed learns it here.
///
/// # Safety
/// `session` must be a live session handle. `out_outcome` must satisfy the
/// versioned-output contract.
#[cfg(feature = "external-frames")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_poll_sync(
    session: *mut MigoSession,
    now_nanos: u64,
    out_outcome: *mut MigoSyncOutcome,
) -> MigoResult {
    guard("migo_session_poll_sync", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        // SAFETY: the versioned-output contract guarantees a readable header.
        if let Err(error) = unsafe {
            migo_capi_abi::validate_header(out_outcome.cast(), size_of::<MigoSyncOutcome>())
        } {
            return error;
        }
        let Ok(state) = session.state.lock() else {
            return MIGO_ERROR_INTERNAL;
        };
        let Some(engine) = state.host.as_ref() else {
            return MIGO_ERROR_INVALID_STATE;
        };
        let snapshot = engine.poll_sync(now_nanos);
        drop(state);
        // SAFETY: forwarded from this function's output contract.
        unsafe { write_sync_snapshot(out_outcome, snapshot) }
    })
}

/// Copy a ready answer out, and free the slot.
///
/// Refused rather than truncated when `capacity` is smaller than the answer,
/// and the answer stays READY so a caller can return with a large enough
/// buffer: a truncated `readPixels` is a wrong answer that looks like a right
/// one. `out_written` receives the byte count on success.
///
/// # Safety
/// `session` must be a live session handle. `buffer` must be writable for
/// `capacity` bytes, or null when `capacity` is zero. `out_written` must be
/// writable.
#[cfg(feature = "external-frames")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_take_sync_reply(
    session: *mut MigoSession,
    buffer: *mut u8,
    capacity: usize,
    out_written: *mut usize,
) -> MigoResult {
    guard("migo_session_take_sync_reply", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        if out_written.is_null() {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        if capacity > isize::MAX as usize {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        if buffer.is_null() && capacity != 0 {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }

        let Ok(state) = session.state.lock() else {
            return MIGO_ERROR_INTERNAL;
        };
        let Some(engine) = state.host.as_ref() else {
            return MIGO_ERROR_INVALID_STATE;
        };
        // SAFETY: null and length were checked above; the contract requires the
        // range to be writable for the call.
        let out = if capacity == 0 {
            &mut [][..]
        } else {
            unsafe { std::slice::from_raw_parts_mut(buffer, capacity) }
        };
        let taken = engine.take_sync_reply(out);
        drop(state);

        match taken {
            Ok(written) => {
                // SAFETY: checked non-null above.
                unsafe { out_written.write(written) };
                MIGO_OK
            }
            Err(_) => {
                // SAFETY: checked non-null above. Zero rather than left alone:
                // a caller that ignores the result must not read a stale count
                // as a byte count.
                unsafe { out_written.write(0) };
                MIGO_ERROR_INVALID_STATE
            }
        }
    })
}

/// The producer withdrew its request.
///
/// # Safety
/// `session` must be a live session handle. `out_outcome` must satisfy the
/// versioned-output contract.
#[cfg(feature = "external-frames")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_cancel_sync(
    session: *mut MigoSession,
    now_nanos: u64,
    out_outcome: *mut MigoSyncOutcome,
) -> MigoResult {
    guard("migo_session_cancel_sync", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        // SAFETY: the versioned-output contract guarantees a readable header.
        if let Err(error) = unsafe {
            migo_capi_abi::validate_header(out_outcome.cast(), size_of::<MigoSyncOutcome>())
        } {
            return error;
        }
        let Ok(state) = session.state.lock() else {
            return MIGO_ERROR_INTERNAL;
        };
        let Some(engine) = state.host.as_ref() else {
            return MIGO_ERROR_INVALID_STATE;
        };
        engine.cancel_sync();
        let snapshot = engine.poll_sync(now_nanos);
        drop(state);
        // SAFETY: forwarded from this function's output contract.
        unsafe { write_sync_snapshot(out_outcome, snapshot) }
    })
}

/// One place that turns a snapshot into the record, so the four entry points
/// cannot disagree about how a state maps.
///
/// # Safety
/// `out` must satisfy the versioned-output contract.
#[cfg(feature = "external-frames")]
unsafe fn write_sync_snapshot(
    out: *mut MigoSyncOutcome,
    snapshot: migo_core::SyncSnapshot,
) -> MigoResult {
    // FREE is the one state the record requires to carry nothing: a cleared
    // mailbox holds a zero id, and reporting the id of a request that has been
    // acknowledged would name one nobody may still ask about.
    let free = snapshot.state == migo_core::SyncState::Free;
    unsafe {
        write_sync_outcome(
            out,
            if free { 0 } else { snapshot.request_id },
            sync_state_code(snapshot.state),
            snapshot.reply_bytes,
            snapshot.error.map(sync_error_code).unwrap_or(0),
        )
    }
}
