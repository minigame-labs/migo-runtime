//! Frames of the recordings a host makes for content.
//!
//! Every recorder operation goes through the host-service channel
//! (`MIGO_HOST_SERVICE_RECORDER`); the frames a recording's options ask for come
//! back here, typed, because a microphone sends many a second. The record is
//! borrowed for the call and its bytes copied once, within the recorder's byte
//! budget: a frame over it is dropped, the last frame never is.

use migo_capi_abi::{
    MIGO_ERROR_INVALID_ARGUMENT, MigoResult,
    host_services::{MIGO_HOST_SERVICE_RECORDER, MigoRecorderFrame},
};

use crate::{MigoSession, map_ingress_result, panic_barrier::guard, pin_session};

/// # Safety
/// `session` must be a live session handle and `frame` null or readable for its
/// announced size, with the bytes it announces readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_post_recorder_frame(
    session: *mut MigoSession,
    frame: *const MigoRecorderFrame,
) -> MigoResult {
    guard("migo_session_post_recorder_frame", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        let frame = match unsafe { MigoRecorderFrame::parse(frame) } {
            Ok(frame) => frame,
            Err(error) => return error,
        };
        let declared = session.state.lock().is_ok_and(|state| {
            state.callbacks.as_ref().is_some_and(|callbacks| {
                callbacks.supplies_host_service(MIGO_HOST_SERVICE_RECORDER)
            })
        });
        if !declared {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        let ingress = match session.active_ingress() {
            Ok(ingress) => ingress,
            Err(error) => return error,
        };
        map_ingress_result(
            &session,
            "migo_session_post_recorder_frame",
            ingress.try_send_recorder_frame(frame.recording_id, frame.data, frame.is_last_frame),
        )
    })
}
