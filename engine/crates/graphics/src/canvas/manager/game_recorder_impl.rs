//! `getGameRecorder` on the canvas manager: the recording it holds, started,
//! paused, stopped and given up here, and the frames it takes at present
//! (`crate::game_recorder`).

use std::{sync::Arc, time::Instant};

use shared::{
    error::{EngineError, ErrorCode},
    game_recorder::GameRecorderCmd,
    protocol::render_cmd::CanvasId,
};

use super::CanvasManager;
use crate::{
    egl_platform::EglConcurrency,
    game_recorder::{Ended, Recording, RecordingContext, VideoEncoderFactory},
};

impl CanvasManager {
    /// The encoder the platform records with, set once after construction.
    pub(crate) fn set_video_encoder(&mut self, factory: Option<Arc<dyn VideoEncoderFactory>>) {
        self.video_encoder = factory;
    }

    /// Whether this device can record the game: an encoder, and a render
    /// backend that shares its frames with another thread's context.
    fn recording_supported(&self) -> bool {
        self.video_encoder
            .as_ref()
            .is_some_and(|factory| factory.available())
            && self.device_caps.tier() == crate::device_caps::DeviceTier::TierA
            && self.egl_provider.concurrency() == EglConcurrency::SharedContexts
    }

    pub(crate) fn game_recorder(&mut self, cmd: GameRecorderCmd) {
        let now = Instant::now();
        self.finishing_recordings
            .retain(|thread| !thread.is_finished());
        match cmd {
            GameRecorderCmd::Supported { resp } => resp.ok(self.recording_supported()),
            GameRecorderCmd::Start {
                params,
                path,
                events,
                resp,
            } => {
                if self.recording.is_some() || !params.is_valid() {
                    resp.err(not_now(
                        "a recording is already being made, or its options are not valid",
                    ));
                    return;
                }
                if !self.recording_supported() {
                    resp.err(
                        EngineError::new(ErrorCode::Unsupported).with_msg("frame not supported"),
                    );
                    return;
                }
                let Some(content) = self
                    .recorded_content()
                    .map(|(_, _, width, height)| (width, height))
                else {
                    resp.err(not_now("there is no frame to record"));
                    return;
                };
                let Some(factory) = self.video_encoder.clone() else {
                    return;
                };
                let cx = RecordingContext {
                    gl: &self.gl,
                    egl_provider: &self.egl_provider,
                    display: self.display,
                    config: self.config,
                    share: self.resource.ctx,
                    gles_major: self.gles_major,
                    features: self.context_features,
                    factory: &factory,
                };
                self.recording = Recording::start(cx, content, params, path, events, resp);
                // Frames are taken from the drawing buffer: while recording,
                // content draws into it rather than straight to the window.
                self.evaluate_bypass();
            }
            GameRecorderCmd::Pause { resp } | GameRecorderCmd::Resume { resp }
                if self.recording.is_none() =>
            {
                resp.err(not_now("no recording is being made"));
            }
            GameRecorderCmd::Pause { resp } => {
                if let Some(recording) = self.recording.as_mut() {
                    recording.pause(true, now);
                }
                resp.ok(());
            }
            GameRecorderCmd::Resume { resp } => {
                if let Some(recording) = self.recording.as_mut() {
                    recording.pause(false, now);
                }
                resp.ok(());
            }
            GameRecorderCmd::Stop { resp } => {
                let Some(recording) = self.recording.take() else {
                    resp.err(not_now("no recording is being made"));
                    return;
                };
                let content = self.recorded_content();
                let finishing = recording.stop(&self.gl, content, now, resp);
                self.finishing_recordings.push(finishing);
                self.evaluate_bypass();
            }
            GameRecorderCmd::Abort { resp } => {
                let Some(recording) = self.recording.take() else {
                    resp.err(not_now("no recording is being made"));
                    return;
                };
                let finishing = recording.abort(&self.gl, Some(resp));
                self.finishing_recordings.push(finishing);
                self.evaluate_bypass();
            }
        }
    }

    /// The onscreen canvas's drawing buffer, current, as a recording reads it:
    /// its framebuffer, whether content turned its read buffer off, its size.
    fn recorded_content(&mut self) -> Option<(glow::NativeFramebuffer, bool, u32, u32)> {
        let onscreen = CanvasId::from(1u32);
        self.make_current_needed(onscreen).ok()?;
        let db = self.canvases.get(&onscreen)?.drawing_buffer.as_ref()?;
        Some((db.fbo, db.content_reads_none, db.width, db.height))
    }

    /// The app went to the background, or came back: a recording's clock does
    /// not run while nothing is presented.
    pub(crate) fn suspend_recording(&mut self, suspended: bool) {
        if let Some(recording) = self.recording.as_mut() {
            recording.suspend(suspended, Instant::now());
        }
    }

    /// At present, with content's frame complete in the drawing buffer and the
    /// window's context current, before the overlay: the recording takes the
    /// frame if one is due.
    pub(crate) fn record_presented_frame(&mut self, onscreen: CanvasId) {
        let Some(recording) = self.recording.as_mut() else {
            return;
        };
        let Some(db) = self
            .canvases
            .get(&onscreen)
            .and_then(|entry| entry.drawing_buffer.as_ref())
        else {
            return;
        };
        let taken = recording.on_present(
            &self.gl,
            db.fbo,
            db.content_reads_none,
            db.width,
            db.height,
            Instant::now(),
        );
        if let Err(Ended::Failed) = taken
            && let Some(recording) = self.recording.take()
        {
            let finishing = recording.ended(&self.gl);
            self.finishing_recordings.push(finishing);
            self.evaluate_bypass();
        }
    }

    /// The share group is going -- lost, or the session is ending: a recording
    /// is given up without touching it, and every encoder thread is joined
    /// before the display they use is.
    pub(crate) fn end_recordings(&mut self, reason: &str) {
        if let Some(recording) = self.recording.take() {
            self.finishing_recordings.push(recording.abandon(reason));
        }
        for thread in self.finishing_recordings.drain(..) {
            thread.join();
        }
    }

    pub(crate) fn recording(&self) -> bool {
        self.recording.is_some()
    }
}

fn not_now(why: &'static str) -> EngineError {
    EngineError::new(ErrorCode::InvalidOperation).with_msg(why)
}
