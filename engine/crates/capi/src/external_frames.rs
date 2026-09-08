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
        MIGO_FRAME_INGRESS_REJECTED, MIGO_FRAME_INGRESS_WOULD_BLOCK, MigoFrameIngressOutcome,
        write_frame_ingress_outcome,
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
