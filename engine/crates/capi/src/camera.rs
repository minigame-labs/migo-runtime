//! Frames from the cameras a host opens for content.
//!
//! Every camera operation goes through the host-service channel
//! (`MIGO_HOST_SERVICE_CAMERA`); the frames of a camera content listens to come
//! back here, typed, because a camera sends thirty a second. The record is
//! borrowed for the call and its pixels read once into content's RGBA, into the
//! mailbox Android's frames take: only the latest waits for content.

use migo_capi_abi::{
    MIGO_ERROR_INVALID_ARGUMENT, MigoResult,
    host_services::{MIGO_HOST_SERVICE_CAMERA, MigoCameraFrame},
};
use shared::protocol::camera_frame::{MAX_CAMERA_FRAME_BYTES, PixelLayout};

use crate::{MigoSession, map_ingress_result, panic_barrier::guard, pin_session};

/// # Safety
/// `session` must be a live session handle and `frame` null or readable for its
/// announced size, with the rows it announces readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn migo_session_post_camera_frame(
    session: *mut MigoSession,
    frame: *const MigoCameraFrame,
) -> MigoResult {
    guard("migo_session_post_camera_frame", || {
        let session = match unsafe { pin_session(session) } {
            Ok(session) => session,
            Err(error) => return error,
        };
        let frame = match unsafe { MigoCameraFrame::parse(frame) } {
            Ok(frame) => frame,
            Err(error) => return error,
        };
        // Content's RGBA, within the budget every synchronous frame keeps to.
        if u64::from(frame.width) * u64::from(frame.height) * 4 > MAX_CAMERA_FRAME_BYTES as u64 {
            return MIGO_ERROR_INVALID_ARGUMENT;
        }
        let declared = session.state.lock().is_ok_and(|state| {
            state
                .callbacks
                .as_ref()
                .is_some_and(|callbacks| callbacks.supplies_host_service(MIGO_HOST_SERVICE_CAMERA))
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
            "migo_session_post_camera_frame",
            ingress.try_send_camera_frame(
                frame.camera_id,
                frame.pixels,
                frame.width,
                frame.height,
                frame.row_stride,
                if frame.bgra {
                    PixelLayout::Bgra
                } else {
                    PixelLayout::Rgba
                },
            ),
        )
    })
}
