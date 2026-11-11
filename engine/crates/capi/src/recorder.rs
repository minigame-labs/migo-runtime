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

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use shared::services::AudioPlatformService;

use crate::{MigoSession, map_ingress_result, panic_barrier::guard, pin_session};

/// The process's audio session, lent to the host's recorder while it records.
///
/// Where the engine owns that session -- iOS, one to a process -- a recording
/// needs a category the engine never sets, and the recorder sets it. So from
/// the START on its way to the host until the host reports that recording over
/// (STOP or ERROR), or the session ends, the engine's audio leaves the session
/// alone, and then puts its own category back. The Session's, as the reports
/// are: a recording outlives a restart of its content no more than the host
/// lets it, and the host says when.
pub(crate) struct RecorderLease {
    audio: Option<Arc<dyn AudioPlatformService>>,
    /// The recording that holds the session; 0 when none does, which no
    /// recording is numbered.
    recording: AtomicU64,
}

impl RecorderLease {
    pub(crate) fn new(audio: Option<Arc<dyn AudioPlatformService>>) -> Self {
        Self {
            audio,
            recording: AtomicU64::new(0),
        }
    }

    /// START for `recording` is on its way to the host. It replaces a recording
    /// under way, which the host stops with no report: the session stays lent.
    pub(crate) fn begin(&self, recording: u64) {
        if self.recording.swap(recording, Ordering::AcqRel) == 0
            && recording != 0
            && let Some(audio) = &self.audio
        {
            audio.recording_began();
        }
    }

    /// The host reported `recording` over, or its START never left. A recording
    /// a later START replaced holds nothing, and its late report ends nothing.
    pub(crate) fn end(&self, recording: u64) {
        if recording != 0
            && self
                .recording
                .compare_exchange(recording, 0, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            && let Some(audio) = &self.audio
        {
            audio.recording_ended();
        }
    }
}

impl Drop for RecorderLease {
    fn drop(&mut self) {
        // The session ended under a recording: the host hears of that through
        // its own teardown and reports nothing more.
        if *self.recording.get_mut() != 0
            && let Some(audio) = &self.audio
        {
            audio.recording_ended();
        }
    }
}

/// The `recordingId` a recorder request or event names; 0 when it names none.
pub(crate) fn recording_id(payload: &serde_json::Value) -> u64 {
    payload
        .get("recordingId")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Session(Mutex<Vec<&'static str>>);

    impl AudioPlatformService for Session {
        fn recording_began(&self) {
            self.0.lock().unwrap().push("began");
        }
        fn recording_ended(&self) {
            self.0.lock().unwrap().push("ended");
        }
    }

    fn lease() -> (Arc<Session>, RecorderLease) {
        let session = Arc::new(Session::default());
        let lease = RecorderLease::new(Some(session.clone() as Arc<dyn AudioPlatformService>));
        (session, lease)
    }

    #[test]
    fn the_session_is_lent_for_a_recording_and_taken_back_when_the_host_reports_it_over() {
        let (session, lease) = lease();
        lease.begin(7);
        assert_eq!(*session.0.lock().unwrap(), ["began"]);
        lease.end(7);
        assert_eq!(*session.0.lock().unwrap(), ["began", "ended"]);
        // Reported twice -- STOP, then an ERROR for the same recording.
        lease.end(7);
        assert_eq!(session.0.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_replaced_recording_s_late_report_does_not_take_the_session_from_the_next() {
        let (session, lease) = lease();
        lease.begin(1);
        lease.begin(2);
        lease.end(1);
        assert_eq!(*session.0.lock().unwrap(), ["began"]);
        lease.end(2);
        assert_eq!(*session.0.lock().unwrap(), ["began", "ended"]);
    }

    #[test]
    fn a_session_that_ends_under_a_recording_gives_the_audio_session_back() {
        let (session, lease) = lease();
        lease.begin(3);
        drop(lease);
        assert_eq!(*session.0.lock().unwrap(), ["began", "ended"]);
        let (session, lease) = lease_idle();
        drop(lease);
        assert!(session.0.lock().unwrap().is_empty());
    }

    fn lease_idle() -> (Arc<Session>, RecorderLease) {
        lease()
    }

    #[test]
    fn a_request_names_its_recording() {
        assert_eq!(recording_id(&serde_json::json!({"recordingId": 9})), 9);
        assert_eq!(recording_id(&serde_json::json!({})), 0);
        assert_eq!(recording_id(&serde_json::json!({"recordingId": "9"})), 0);
    }
}
