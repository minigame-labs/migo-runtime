//! `getGameRecorder`: recording the game's own frames into a video.
//!
//! Content's calls are judged by the JavaScript side, which owns the
//! reference's error codes and answers its state and option errors itself;
//! what reaches the render thread is a recording to make. The render thread captures the frame content drew --
//! before the engine's overlay, so the native buttons are never in it -- at the
//! recording's rate, and an encoder thread turns the captures into the file.
//! What the recording does on its own (its clock moving on, a failure) reaches
//! content through [`RecorderEvents`], which never loses a failure and keeps
//! only the latest time.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use tokio::sync::Notify;

use crate::protocol::render_cmd::RenderCmdResp;

/// What a recording is made with: `GameRecorder.start`'s options, judged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordingParams {
    /// Frames a second, 1 to 60.
    pub fps: u32,
    /// Kilobits a second, 600 to 3000.
    pub bitrate_kbps: u32,
    /// Frames from one key frame to the next.
    pub gop: u32,
    /// How much is recorded at most; the recording then stops taking frames
    /// but goes on until content stops it, as the reference has it.
    pub duration_ms: u64,
}

impl RecordingParams {
    pub const MIN_FPS: u32 = 1;
    pub const MAX_FPS: u32 = 60;
    pub const MIN_BITRATE_KBPS: u32 = 600;
    pub const MAX_BITRATE_KBPS: u32 = 3000;
    pub const MIN_DURATION_MS: u64 = 5_000;
    pub const MAX_DURATION_MS: u64 = 7_200_000;

    /// Whether every option is one the reference allows: what reaches the
    /// render thread was judged by content's side first.
    pub fn is_valid(&self) -> bool {
        (Self::MIN_FPS..=Self::MAX_FPS).contains(&self.fps)
            && (Self::MIN_BITRATE_KBPS..=Self::MAX_BITRATE_KBPS).contains(&self.bitrate_kbps)
            && self.gop >= 1
            && (Self::MIN_DURATION_MS..=Self::MAX_DURATION_MS).contains(&self.duration_ms)
    }
}

/// What content asks of the render thread's recorder.
pub enum GameRecorderCmd {
    /// Whether this device can record the game: a render backend that can
    /// share its frames with an encoder thread, and an encoder.
    Supported {
        resp: RenderCmdResp<bool>,
    },
    /// Start recording into `path`, replacing whatever was there; answered once
    /// frames are being taken. `events` hears the recording until it ends.
    Start {
        params: RecordingParams,
        path: PathBuf,
        events: Arc<RecorderEvents>,
        resp: RenderCmdResp<()>,
    },
    /// Stop taking frames; the recording's clock stops with them.
    Pause {
        resp: RenderCmdResp<()>,
    },
    Resume {
        resp: RenderCmdResp<()>,
    },
    /// End the recording and finish its file, answered with what it holds in
    /// milliseconds once the file is complete.
    Stop {
        resp: RenderCmdResp<u64>,
    },
    /// End the recording and remove its file.
    Abort {
        resp: RenderCmdResp<()>,
    },
}

impl std::fmt::Debug for GameRecorderCmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Supported { .. } => "Supported",
            Self::Start { .. } => "Start",
            Self::Pause { .. } => "Pause",
            Self::Resume { .. } => "Resume",
            Self::Stop { .. } => "Stop",
            Self::Abort { .. } => "Abort",
        })
    }
}

/// What a recording reports on its own, in the order content hears it.
#[derive(Clone, Debug, PartialEq)]
pub enum RecorderEvent {
    /// How much has been recorded, in milliseconds.
    Time(u64),
    /// The recording failed and ended; its file is gone.
    Failed(String),
    /// No more events: the recording ended, or a new one replaced it.
    Closed,
}

/// The events of one recording, from the threads making it to content's
/// waiting op. A failure is never lost; times coalesce to the latest, since
/// each supersedes the one before; after [`RecorderEvent::Closed`] nothing
/// more arrives.
#[derive(Debug, Default)]
pub struct RecorderEvents {
    pending: Mutex<Pending>,
    ready: Notify,
}

#[derive(Debug, Default)]
struct Pending {
    time_ms: Option<u64>,
    failure: Option<String>,
    closed: bool,
}

impl RecorderEvents {
    /// The recording has reached `time_ms`.
    pub fn time(&self, time_ms: u64) {
        {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            if pending.closed {
                return;
            }
            pending.time_ms = Some(time_ms);
        }
        self.ready.notify_one();
    }

    /// The recording failed; nothing follows but [`RecorderEvent::Closed`].
    pub fn fail(&self, reason: impl Into<String>) {
        {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            if pending.closed {
                return;
            }
            pending.failure.get_or_insert_with(|| reason.into());
            pending.closed = true;
        }
        self.ready.notify_one();
    }

    /// The recording ended.
    pub fn close(&self) {
        {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            pending.closed = true;
        }
        self.ready.notify_one();
    }

    /// What happened since the last call, waiting until something has: a
    /// failure first, then the latest time, then the end.
    pub async fn next(&self) -> RecorderEvent {
        loop {
            let ready = self.ready.notified();
            if let Some(event) = self.take() {
                return event;
            }
            ready.await;
        }
    }

    fn take(&self) -> Option<RecorderEvent> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(reason) = pending.failure.take() {
            pending.time_ms = None;
            return Some(RecorderEvent::Failed(reason));
        }
        if let Some(time_ms) = pending.time_ms.take() {
            return Some(RecorderEvent::Time(time_ms));
        }
        pending.closed.then_some(RecorderEvent::Closed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now(events: &RecorderEvents) -> Option<RecorderEvent> {
        events.take()
    }

    #[test]
    fn times_coalesce_to_the_latest_and_a_failure_comes_first() {
        let events = RecorderEvents::default();
        events.time(250);
        events.time(500);
        assert_eq!(now(&events), Some(RecorderEvent::Time(500)));
        assert_eq!(now(&events), None);
        events.time(750);
        events.fail("encoder");
        events.time(1000);
        assert_eq!(now(&events), Some(RecorderEvent::Failed("encoder".into())));
        assert_eq!(now(&events), Some(RecorderEvent::Closed));
        assert_eq!(now(&events), Some(RecorderEvent::Closed));
    }

    #[test]
    fn a_waiter_hears_what_was_reported_before_it_waited() {
        let events = std::sync::Arc::new(RecorderEvents::default());
        events.time(40);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        assert_eq!(runtime.block_on(events.next()), RecorderEvent::Time(40));
        let closer = std::sync::Arc::clone(&events);
        std::thread::spawn(move || closer.close());
        assert_eq!(runtime.block_on(events.next()), RecorderEvent::Closed);
    }

    #[test]
    fn only_the_options_the_reference_allows_are_valid() {
        let ok = RecordingParams {
            fps: 24,
            bitrate_kbps: 1000,
            gop: 12,
            duration_ms: 7_200_000,
        };
        assert!(ok.is_valid());
        assert!(!RecordingParams { fps: 0, ..ok }.is_valid());
        assert!(
            !RecordingParams {
                bitrate_kbps: 599,
                ..ok
            }
            .is_valid()
        );
        assert!(!RecordingParams { gop: 0, ..ok }.is_valid());
        assert!(
            !RecordingParams {
                duration_ms: 4_999,
                ..ok
            }
            .is_valid()
        );
    }
}
