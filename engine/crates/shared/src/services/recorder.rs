//! The microphone: `getRecorderManager`'s recordings and their frames.

use crate::protocol::error::ServiceError;
use crate::services::host_files::HostFiles;

/// The microphone: one recording at a time, its file and its frames.
///
/// The reference's recorder is driven by commands and heard through events:
/// `start`, `pause`, `resume` and `stop` answer nothing, and what happens --
/// the recording started, paused, stopped with its file, failed, was
/// interrupted -- reaches content through [`RECORDER_EVENT_HOOKS`]. Every
/// request and every event carries the recording's `recordingId`, which content
/// takes from the Host id space when it starts one: an event for a recording
/// content no longer has -- the runtime that started it was replaced, or a
/// later recording took its place -- names an id content does not hold, and
/// is dropped. Frames, while the options ask for them, are a typed path of
/// their own (`protocol::recorder_frame`), carrying the same id.
pub trait RecorderService: Send + Sync {
    /// Start recording `recordingId` with content's options: `duration` (ms,
    /// up to 600000), `sampleRate`, `numberOfChannels`, `encodeBitRate`,
    /// `format` ("mp3", "aac", "wav", "PCM"), `frameSize` (KB; frames are
    /// handed over only when it is set), `audioSource` and `audioMode`. A
    /// command: the recording reports START once it records, or ERROR.
    fn start(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "recorderManager.start:fail not supported",
        ))
    }

    /// Pause recording `recordingId`. A command: PAUSE once paused.
    fn pause(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "recorderManager.pause:fail not supported",
        ))
    }

    /// Resume recording `recordingId`. A command: RESUME once it records again.
    fn resume(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "recorderManager.resume:fail not supported",
        ))
    }

    /// Stop recording `recordingId`. A command: STOP with the file once it is
    /// written.
    fn stop(&self, _request_json: &str) -> Result<(), ServiceError> {
        Err(ServiceError::not_supported(
            "recorderManager.stop:fail not supported",
        ))
    }
}

/// The recorder's commands by number (`MIGO_RECORDER_*` in
/// `include/migo/host_services.h`).
pub mod recorder_method {
    pub const START: u32 = 0;
    pub const PAUSE: u32 = 1;
    pub const RESUME: u32 = 2;
    pub const STOP: u32 = 3;
}

/// The recorder's events by number (`MIGO_RECORDER_EVENT_*`).
pub mod recorder_event {
    pub const START: u32 = 0;
    pub const PAUSE: u32 = 1;
    pub const RESUME: u32 = 2;
    pub const STOP: u32 = 3;
    pub const ERROR: u32 = 4;
    pub const INTERRUPTION_BEGIN: u32 = 5;
    pub const INTERRUPTION_END: u32 = 6;
}

/// The hook each recorder event reaches content through, indexed by its event
/// number; each with `{"recordingId"}`, STOP with `tempFilePath`, `duration`
/// and `fileSize`, ERROR with `errMsg`. One table for every SDK.
pub const RECORDER_EVENT_HOOKS: [&str; 7] = [
    "_internalOnRecorderStartEvent",
    "_internalOnRecorderPauseEvent",
    "_internalOnRecorderResumeEvent",
    "_internalOnRecorderStopEvent",
    "_internalOnRecorderErrorEvent",
    "_internalOnRecorderInterruptionBeginEvent",
    "_internalOnRecorderInterruptionEndEvent",
];

/// The files each recorder event hands over, by its event number: STOP's
/// recording. A host may stop on its own -- the duration ran out -- so the file
/// travels with the event rather than with an answer to `stop`.
pub const fn recorder_event_files(event: u32) -> &'static HostFiles {
    match event {
        recorder_event::STOP => &HostFiles::RECORDER_STOP,
        _ => &HostFiles::NONE,
    }
}
