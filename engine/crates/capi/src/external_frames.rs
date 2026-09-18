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
        MIGO_FRAME_INGRESS_ACCEPTED, MIGO_FRAME_INGRESS_DEFERRED,
        MIGO_FRAME_INGRESS_GENERATION_LOST, MIGO_FRAME_INGRESS_REJECTED,
        MIGO_FRAME_INGRESS_WOULD_BLOCK, MIGO_SYNC_ERROR_ALREADY_PENDING,
        MIGO_SYNC_ERROR_BAD_DEADLINE, MIGO_SYNC_ERROR_BAD_REPLY_RESERVATION,
        MIGO_SYNC_ERROR_LATE_REPLY, MIGO_SYNC_ERROR_OPERATION_FAILED,
        MIGO_SYNC_ERROR_REPLY_TOO_LARGE, MIGO_SYNC_ERROR_REQUEST_ID_MISMATCH,
        MIGO_SYNC_ERROR_SESSION_ENDED, MIGO_SYNC_ERROR_STALE_GENERATION, MIGO_SYNC_ERROR_TIMED_OUT,
        MIGO_SYNC_ERROR_UNSUPPORTED_OPERATION, MIGO_SYNC_STATE_CANCELLED, MIGO_SYNC_STATE_FAILED,
        MIGO_SYNC_STATE_FREE, MIGO_SYNC_STATE_PENDING, MIGO_SYNC_STATE_READY,
        MIGO_UPLINK_MESSAGE_CONTROL, MIGO_UPLINK_MESSAGE_FRAME, MigoDownlinkWakerFn,
        MigoFrameIngressOutcome, MigoSyncOutcome, MigoSyncRequestDescriptor, MigoUplinkMessageKind,
        write_frame_ingress_outcome, write_sync_outcome,
    },
};
use migo_core::IngressDecision;

// The C ABI's operation numbers and reply size are the wire's, asserted at
// compile time: a renumbering on either side is a build failure rather than a
// producer blocked on an operation the library dispatches as another.
const _: () = {
    assert!(
        migo_capi_abi::external_frames::MIGO_SYNC_OP_READ_PIXELS == migo_core::SYNC_OP_READ_PIXELS
    );
    assert!(
        migo_capi_abi::external_frames::MIGO_SYNC_OP_AWAIT_WINDOW
            == migo_core::SYNC_OP_AWAIT_WINDOW
    );
    assert!(
        migo_capi_abi::external_frames::MIGO_SYNC_WINDOW_REPLY_BYTES as usize
            == migo_core::WINDOW_REPLY_BYTES
    );
};

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
            IngressDecision::Deferred => MIGO_FRAME_INGRESS_DEFERRED,
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
/// vsync rather than by a browser. A request made before the renderer is up is
/// held and armed when it starts, so both outcomes are `MIGO_OK`; the state
/// error is for a session with no surface, which has no clock at all.
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
        // Armed or held, the request is not lost, and neither is something
        // the host has to act on.
        let _ = engine.clock().request_frame();
        MIGO_OK
    })
}

/// Which door a message from the producer's socket goes through.
///
/// # Safety
/// `bytes` must be readable for `byte_count` bytes, or null when `byte_count` is
/// zero. `out_kind` must be writable.
#[cfg(feature = "external-frames")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_uplink_message_kind(
    bytes: *const u8,
    byte_count: usize,
    out_kind: *mut MigoUplinkMessageKind,
) -> MigoResult {
    guard("migo_uplink_message_kind", || {
        if out_kind.is_null() || (bytes.is_null() && byte_count != 0) {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        if byte_count > isize::MAX as usize {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        let message = if byte_count == 0 {
            &[][..]
        } else {
            // SAFETY: null and length were checked above; the contract requires
            // the range to be readable for the call.
            unsafe { std::slice::from_raw_parts(bytes, byte_count) }
        };
        let kind = if migo_core::is_control_message(message) {
            MIGO_UPLINK_MESSAGE_CONTROL
        } else {
            MIGO_UPLINK_MESSAGE_FRAME
        };
        // SAFETY: checked non-null above.
        unsafe { out_kind.write(kind) };
        MIGO_OK
    })
}

/// Read one control message from the producer and act on it.
///
/// `*out_refusal_code` is zero when the message was read, including when its
/// requests belonged to another generation, and otherwise the stable code of
/// the rule it broke. The call's own result is about whether the call could be
/// made, exactly as for `migo_session_submit_external_frame`.
///
/// # Safety
/// `session` must be a live session handle. `bytes` must be readable for
/// `byte_count` bytes. `out_refusal_code` must be writable.
#[cfg(feature = "external-frames")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_submit_uplink_control(
    session: *mut MigoSession,
    bytes: *const u8,
    byte_count: usize,
    out_refusal_code: *mut u32,
) -> MigoResult {
    guard("migo_session_submit_uplink_control", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        if bytes.is_null() || byte_count == 0 || out_refusal_code.is_null() {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        if byte_count > isize::MAX as usize {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        let Ok(state) = session.state.lock() else {
            return MIGO_ERROR_INTERNAL;
        };
        let Some(engine) = state.host.as_ref() else {
            return MIGO_ERROR_INVALID_STATE;
        };
        // SAFETY: null and length were checked above; nothing derived from the
        // slice outlives this call.
        let message = unsafe { std::slice::from_raw_parts(bytes, byte_count) };
        let refusal = match engine.submit_control(message) {
            Ok(_) => 0,
            Err(error) => error.code(),
        };
        drop(state);
        // SAFETY: checked non-null above.
        unsafe { out_refusal_code.write(refusal) };
        MIGO_OK
    })
}

/// A C waker and the pointer it is called with, as the session calls it.
#[cfg(feature = "external-frames")]
struct CWaker {
    waker: MigoDownlinkWakerFn,
    user_data: *mut std::ffi::c_void,
}

// SAFETY: the pointer is the host's and is only ever handed back to the host's
// own function; the header makes the host responsible for it being usable from
// the session thread, which is the only thread this is called on.
#[cfg(feature = "external-frames")]
unsafe impl Send for CWaker {}
#[cfg(feature = "external-frames")]
unsafe impl Sync for CWaker {}

#[cfg(feature = "external-frames")]
impl CWaker {
    /// A method rather than a field call in the closure below: a closure names
    /// fields it uses and would capture the raw pointer on its own, which is
    /// neither `Send` nor `Sync`. Through `&self` it captures this struct.
    fn wake(&self) {
        // The host's function is called directly rather than through the panic
        // barrier: it is C, it cannot unwind into Rust, and a barrier here
        // would cost a closure per tick for nothing.
        // SAFETY: the host's contract for this waker, in the header.
        unsafe { (self.waker)(self.user_data) }
    }
}

/// Install or clear the function called when a tick is queued.
///
/// # Safety
/// `session` must be a live session handle. `waker`, when non-null, must be
/// callable from any thread with `user_data` until it is cleared or replaced.
#[cfg(feature = "external-frames")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_set_downlink_waker(
    session: *mut MigoSession,
    waker: Option<MigoDownlinkWakerFn>,
    user_data: *mut std::ffi::c_void,
) -> MigoResult {
    guard("migo_session_set_downlink_waker", || {
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
        let installed = waker.map(|waker| {
            let target = CWaker { waker, user_data };
            Box::new(move || target.wake()) as migo_core::DownlinkWaker
        });
        // Under the session's state lock, which the session thread never takes,
        // so waiting here for a call in progress cannot deadlock with it.
        engine.set_downlink_waker(installed);
        MIGO_OK
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
    /// A call whose header or reply has nowhere to go is refused before the
    /// session is even consulted, and leaves no pointer behind.
    #[test]
    fn a_call_with_nowhere_to_answer_is_refused_before_it_is_posted() {
        with_session("external-call-sync-arguments", |session| {
            let body = [0u8; 80];
            let mut header = [0u8; 16];
            assert_eq!(
                unsafe {
                    migo_session_call_sync(
                        session,
                        body.as_ptr(),
                        body.len(),
                        0,
                        header.as_mut_ptr(),
                        header.len(),
                        std::ptr::null_mut(),
                    )
                },
                MIGO_ERROR_INVALID_ARGUMENT
            );

            // A stale pointer in the output, as a caller reusing a variable
            // would leave: it must come back null, not be left to be released.
            let mut reply = std::ptr::NonNull::<MigoSyncReply>::dangling().as_ptr();
            assert_eq!(
                unsafe {
                    migo_session_call_sync(
                        session,
                        body.as_ptr(),
                        body.len(),
                        0,
                        header.as_mut_ptr(),
                        migo_core::SYNC_ANSWER_HEADER_BYTES - 1,
                        &mut reply,
                    )
                },
                MIGO_ERROR_INVALID_ARGUMENT
            );
            assert!(reply.is_null());
            assert_eq!(
                unsafe {
                    migo_session_call_sync(
                        session,
                        body.as_ptr(),
                        body.len(),
                        0,
                        std::ptr::null_mut(),
                        16,
                        &mut reply,
                    )
                },
                MIGO_ERROR_INVALID_ARGUMENT
            );
            assert_eq!(
                unsafe {
                    migo_session_call_sync(
                        session,
                        std::ptr::null(),
                        body.len(),
                        0,
                        header.as_mut_ptr(),
                        header.len(),
                        &mut reply,
                    )
                },
                MIGO_ERROR_INVALID_ARGUMENT
            );
            assert!(reply.is_null());
        });
    }

    /// No renderer, no answer: a state error the transport reports as such,
    /// rather than a header it would send as a verdict.
    #[test]
    fn a_call_before_a_surface_is_attached_is_a_state_error() {
        with_session("external-call-sync-no-surface", |session| {
            let body = [0u8; 80];
            let mut header = [0xAAu8; 16];
            let mut reply = std::ptr::null_mut();
            assert_eq!(
                unsafe {
                    migo_session_call_sync(
                        session,
                        body.as_ptr(),
                        body.len(),
                        0,
                        header.as_mut_ptr(),
                        header.len(),
                        &mut reply,
                    )
                },
                MIGO_ERROR_INVALID_STATE
            );
            assert!(reply.is_null());
            assert_eq!(header, [0xAA; 16], "no header was written for no answer");
        });
    }

    /// The handle gives back exactly the bytes it was made from, at the address
    /// they were allocated at -- the renderer's buffer, not a copy of it.
    #[test]
    fn a_reply_hands_over_its_own_bytes_and_is_freed_once() {
        let pixels: Vec<u8> = (0..=255).collect();
        let address = pixels.as_ptr();
        let reply = Box::into_raw(Box::new(MigoSyncReply(pixels)));

        let mut bytes = std::ptr::null();
        let mut length = usize::MAX;
        assert_eq!(
            unsafe { migo_sync_reply_bytes(reply, &mut bytes, &mut length) },
            MIGO_OK
        );
        assert_eq!((bytes, length), (address, 256));
        let copied = unsafe { std::slice::from_raw_parts(bytes, length) };
        assert!(copied.iter().enumerate().all(|(i, &b)| b == i as u8));

        assert_eq!(unsafe { migo_sync_reply_release(reply) }, MIGO_OK);
    }

    #[test]
    fn a_null_reply_is_refused_and_clears_what_it_would_have_written() {
        let mut bytes = [0u8; 1].as_ptr();
        let mut length = 7usize;
        assert_eq!(
            unsafe { migo_sync_reply_bytes(std::ptr::null(), &mut bytes, &mut length) },
            MIGO_ERROR_INVALID_ARGUMENT
        );
        assert_eq!((bytes, length), (std::ptr::null(), 0));
        assert_eq!(
            unsafe { migo_sync_reply_release(std::ptr::null_mut()) },
            MIGO_ERROR_INVALID_ARGUMENT
        );
    }

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
        let body = [0u8; 80];
        let mut header = [0u8; 16];
        let mut reply = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                migo_session_call_sync(
                    std::ptr::null_mut(),
                    body.as_ptr(),
                    body.len(),
                    0,
                    header.as_mut_ptr(),
                    header.len(),
                    &mut reply,
                )
            },
            MIGO_ERROR_INVALID_ARGUMENT
        );
        assert!(reply.is_null());
    }

    #[test]
    fn asking_for_a_frame_before_a_surface_is_attached_is_a_state_error() {
        with_session("external-request-frame", |session| {
            assert_eq!(
                unsafe { migo_session_request_external_frame(session) },
                MIGO_ERROR_INVALID_STATE
            );
        });
    }

    /// A session with a started engine and no renderer, which is the state a
    /// producer's first request races.
    fn with_engine_installed(tag: &str, body: impl FnOnce(*mut MigoSession)) {
        with_session(tag, |session| {
            {
                let pinned = unsafe { &*session };
                let mut state = pinned.state.lock().expect("SessionControl");
                let join = std::thread::Builder::new()
                    .name(format!("Migo-Main-{tag}"))
                    .spawn(|| {})
                    .expect("spawn inert test Host");
                state.host = Some(crate::session_engine::engine_for_test(i32::MAX - 1, join));
            }
            body(session);
            // Taken back out before the session is destroyed, which refuses a
            // session whose engine it did not start and cannot retire.
            let host = unsafe { &*session }
                .state
                .lock()
                .expect("SessionControl")
                .host
                .take();
            drop(host);
        });
    }

    fn control(generation: u32) -> Vec<u8> {
        migo_core::encode_control(&[migo_core::ControlRecord::RequestFrame { generation }])
    }

    #[test]
    fn the_router_is_told_which_door_a_message_goes_through() {
        let mut kind = 0;
        let request = control(1);
        assert_eq!(
            unsafe { migo_uplink_message_kind(request.as_ptr(), request.len(), &mut kind) },
            MIGO_OK
        );
        assert_eq!(kind, MIGO_UPLINK_MESSAGE_CONTROL);

        let frame = [0x46u8, 0x50, 0x47, 0x4D, 1, 0, 0, 0];
        assert_eq!(
            unsafe { migo_uplink_message_kind(frame.as_ptr(), frame.len(), &mut kind) },
            MIGO_OK
        );
        assert_eq!(
            kind, MIGO_UPLINK_MESSAGE_FRAME,
            "a frame goes to the frame door"
        );

        assert_eq!(
            unsafe { migo_uplink_message_kind(std::ptr::null(), 0, &mut kind) },
            MIGO_OK
        );
        assert_eq!(
            kind, MIGO_UPLINK_MESSAGE_FRAME,
            "so does nothing at all: frame ingress is what refuses it"
        );
        assert_eq!(
            unsafe { migo_uplink_message_kind(std::ptr::null(), 4, &mut kind) },
            MIGO_ERROR_INVALID_ARGUMENT
        );
        assert_eq!(
            unsafe { migo_uplink_message_kind(request.as_ptr(), 4, std::ptr::null_mut()) },
            MIGO_ERROR_INVALID_ARGUMENT
        );
    }

    #[test]
    fn a_control_message_needs_a_surface_and_somewhere_to_answer() {
        let request = control(1);
        let mut refusal = u32::MAX;
        assert_eq!(
            unsafe {
                migo_session_submit_uplink_control(
                    std::ptr::null_mut(),
                    request.as_ptr(),
                    request.len(),
                    &mut refusal,
                )
            },
            MIGO_ERROR_INVALID_ARGUMENT
        );
        with_session("external-control-no-surface", |session| {
            assert_eq!(
                unsafe {
                    migo_session_submit_uplink_control(
                        session,
                        request.as_ptr(),
                        request.len(),
                        &mut refusal,
                    )
                },
                MIGO_ERROR_INVALID_STATE
            );
            assert_eq!(
                unsafe {
                    migo_session_submit_uplink_control(
                        session,
                        request.as_ptr(),
                        request.len(),
                        std::ptr::null_mut(),
                    )
                },
                MIGO_ERROR_INVALID_ARGUMENT
            );
        });
        assert_eq!(
            refusal,
            u32::MAX,
            "nothing was written by a call that was not made"
        );
    }

    #[test]
    fn a_control_message_is_read_or_refused_with_the_rule_it_broke() {
        with_engine_installed("external-control-read", |session| {
            let mut refusal = u32::MAX;
            let request = control(1);
            assert_eq!(
                unsafe {
                    migo_session_submit_uplink_control(
                        session,
                        request.as_ptr(),
                        request.len(),
                        &mut refusal,
                    )
                },
                MIGO_OK
            );
            assert_eq!(
                refusal, 0,
                "a request before the renderer is up is held, not refused"
            );

            let stale = control(9);
            assert_eq!(
                unsafe {
                    migo_session_submit_uplink_control(
                        session,
                        stale.as_ptr(),
                        stale.len(),
                        &mut refusal,
                    )
                },
                MIGO_OK
            );
            assert_eq!(
                refusal, 0,
                "another generation's request is ignored, not refused"
            );

            let mut malformed = control(1);
            malformed.push(0);
            assert_eq!(
                unsafe {
                    migo_session_submit_uplink_control(
                        session,
                        malformed.as_ptr(),
                        malformed.len(),
                        &mut refusal,
                    )
                },
                MIGO_OK
            );
            assert_eq!(refusal, migo_core::ControlError::TrailingBytes.code());
        });
    }

    unsafe extern "C" fn count_wake(user_data: *mut std::ffi::c_void) {
        let counter = unsafe { &*(user_data as *const std::sync::atomic::AtomicU32) };
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    #[test]
    fn a_waker_needs_a_surface_and_can_be_installed_and_cleared() {
        let counter = std::sync::atomic::AtomicU32::new(0);
        let user_data = &counter as *const _ as *mut std::ffi::c_void;
        assert_eq!(
            unsafe {
                migo_session_set_downlink_waker(std::ptr::null_mut(), Some(count_wake), user_data)
            },
            MIGO_ERROR_INVALID_ARGUMENT
        );
        with_session("external-waker-no-surface", |session| {
            assert_eq!(
                unsafe { migo_session_set_downlink_waker(session, Some(count_wake), user_data) },
                MIGO_ERROR_INVALID_STATE
            );
        });
        with_engine_installed("external-waker", |session| {
            assert_eq!(
                unsafe { migo_session_set_downlink_waker(session, Some(count_wake), user_data) },
                MIGO_OK
            );
            assert_eq!(
                unsafe { migo_session_set_downlink_waker(session, None, std::ptr::null_mut()) },
                MIGO_OK
            );
        });
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "installing is not a tick"
        );
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
        E::OperationFailed => MIGO_SYNC_ERROR_OPERATION_FAILED,
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

        // The handle is taken under the session lock and the lock is released
        // before the request is answered. Answering blocks -- for the frame the
        // request names, then for the readback -- and that frame arrives through
        // migo_session_submit_external_frame, which needs this lock. Holding it
        // here would make the read wait for a frame that is waiting for the read.
        let sync = {
            let Ok(state) = session.state.lock() else {
                return MIGO_ERROR_INTERNAL;
            };
            let Some(engine) = state.host.as_ref() else {
                return MIGO_ERROR_INVALID_STATE;
            };
            engine.sync_handle()
        };

        let posted = sync.post(
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
        // The post reports where its own request ended. A poll here instead
        // would read whatever the mailbox holds by then, and with the session
        // lock released that can be the next request.
        let snapshot = match posted {
            Ok(snapshot) => snapshot,
            Err(error) => {
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

        // SAFETY: forwarded from this function's output contract.
        unsafe { write_sync_snapshot(out_outcome, snapshot) }
    })
}

/// The reply to a one-body synchronous call: the bytes the renderer answered
/// with, owned here until the host releases them.
///
/// Opaque so the bytes cross the boundary by ownership rather than by copy.
/// The vector is kept as it came -- not shrunk to a boxed slice, because a
/// shrink that cannot happen in place is a reallocation and a copy, which is
/// what #250 measured on macOS.
#[cfg(feature = "external-frames")]
pub struct MigoSyncReply(Vec<u8>);

/// Answer a synchronous call carried whole in one body.
///
/// # Safety
/// `session` must be a live session handle. `call` must be readable for
/// `call_bytes` bytes, or null with `call_bytes` zero. `header` must be
/// writable for `header_capacity` bytes. `out_reply` must be writable for one
/// pointer.
#[cfg(feature = "external-frames")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_call_sync(
    session: *mut MigoSession,
    call: *const u8,
    call_bytes: usize,
    now_nanos: u64,
    header: *mut u8,
    header_capacity: usize,
    out_reply: *mut *mut MigoSyncReply,
) -> MigoResult {
    guard("migo_session_call_sync", || {
        let Some(out_reply) = (unsafe { out_reply.as_mut() }) else {
            return MIGO_ERROR_INVALID_ARGUMENT;
        };
        // Cleared before anything can fail, so a caller that ignores the result
        // cannot release, or send, a pointer it never received.
        *out_reply = std::ptr::null_mut();
        // Checked before the call is posted: a header with nowhere to go is an
        // answer that could not be delivered, and nothing should be read back
        // for it.
        if header.is_null() || header_capacity < migo_core::SYNC_ANSWER_HEADER_BYTES {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        let Some(body) = (unsafe { call_body(call, call_bytes) }) else {
            return MIGO_ERROR_INVALID_ARGUMENT;
        };

        // Taken under the lock, used outside it, for the reason
        // migo_session_post_sync_request gives: answering waits for a frame
        // that arrives through a call needing this lock.
        let sync = {
            let Ok(state) = session.state.lock() else {
                return MIGO_ERROR_INTERNAL;
            };
            let Some(engine) = state.host.as_ref() else {
                return MIGO_ERROR_INVALID_STATE;
            };
            engine.sync_handle()
        };

        let answered = sync.answer(body, now_nanos);
        // SAFETY: non-null and at least SYNC_ANSWER_HEADER_BYTES long, checked
        // above; nothing derived from it outlives this call.
        let header =
            unsafe { std::slice::from_raw_parts_mut(header, migo_core::SYNC_ANSWER_HEADER_BYTES) };
        answered.answer.write_header(header);
        if !answered.reply.is_empty() {
            *out_reply = Box::into_raw(Box::new(MigoSyncReply(answered.reply)));
        }
        MIGO_OK
    })
}

/// Where a reply's bytes are.
///
/// # Safety
/// `reply` must be a live handle from [`migo_session_call_sync`]. `out_bytes`
/// and `out_length` must be writable. The bytes are valid until the handle is
/// released.
#[cfg(feature = "external-frames")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_sync_reply_bytes(
    reply: *const MigoSyncReply,
    out_bytes: *mut *const u8,
    out_length: *mut usize,
) -> MigoResult {
    guard("migo_sync_reply_bytes", || {
        let (Some(out_bytes), Some(out_length)) = (unsafe { out_bytes.as_mut() }, unsafe {
            out_length.as_mut()
        }) else {
            return MIGO_ERROR_INVALID_ARGUMENT;
        };
        *out_bytes = std::ptr::null();
        *out_length = 0;
        let Some(reply) = (unsafe { reply.as_ref() }) else {
            return MIGO_ERROR_INVALID_ARGUMENT;
        };
        *out_bytes = reply.0.as_ptr();
        *out_length = reply.0.len();
        MIGO_OK
    })
}

/// Free a reply.
///
/// # Safety
/// `reply` must be a unique live handle from [`migo_session_call_sync`], or
/// null. It is invalid afterwards.
#[cfg(feature = "external-frames")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_sync_reply_release(reply: *mut MigoSyncReply) -> MigoResult {
    guard("migo_sync_reply_release", || {
        if reply.is_null() {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        // SAFETY: the caller hands back the unique handle this library boxed.
        drop(unsafe { Box::from_raw(reply) });
        MIGO_OK
    })
}

/// A call body as a slice, or `None` for a pointer and length that cannot be one.
///
/// # Safety
/// `call` must be readable for `call_bytes` bytes, or null with `call_bytes` zero.
#[cfg(feature = "external-frames")]
unsafe fn call_body<'a>(call: *const u8, call_bytes: usize) -> Option<&'a [u8]> {
    if call_bytes > isize::MAX as usize || (call.is_null() && call_bytes != 0) {
        return None;
    }
    if call_bytes == 0 {
        return Some(&[]);
    }
    // SAFETY: non-null and bounded, per the caller's contract.
    Some(unsafe { std::slice::from_raw_parts(call, call_bytes) })
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

/// Take the next message the host owes the producer.
///
/// Writes at most `capacity` bytes into `buffer` and reports the length in
/// `out_written`. A length of zero means there is nothing to send, which is the
/// normal answer between frames -- not an error, and not something to retry in a
/// loop.
///
/// The message is a downlink envelope: per-frame verdicts and frame-clock
/// ticks, in the format `engine/crates/frame-wire/src/downlink.rs` specifies and
/// the producer's `downlink.mjs` reads. The host does not build it. That is the
/// point of this entry point rather than an accessor per field: a third
/// implementation of a wire format is the drift this repository already keeps a
/// gate for, and a transport that only copies bytes cannot drift.
///
/// Whole records only. A `capacity` too small for the envelope plus one record
/// writes nothing and keeps everything queued, so a caller that grows its buffer
/// and asks again loses nothing.
///
/// # Safety
/// `session` must be a live session handle. `buffer` must be writable for
/// `capacity` bytes when `capacity` is non-zero, and `out_written` must be a
/// writable `usize`.
#[cfg(feature = "external-frames")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_take_downlink(
    session: *mut MigoSession,
    buffer: *mut u8,
    capacity: usize,
    out_written: *mut usize,
) -> MigoResult {
    guard("migo_session_take_downlink", || {
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
        let written = engine.take_downlink(out);
        drop(state);

        // SAFETY: checked non-null above.
        unsafe { out_written.write(written) };
        MIGO_OK
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
