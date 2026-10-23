//! `getGameRecorder`'s requests, as both executions make them: the embedded
//! runtime's ops and the Performance+ host's answers to its producer. Content's
//! calls are judged in `05_game_recorder.js`, which owns the reference's error
//! codes; these ask the render thread and answer with what happened -- started,
//! not supported, failed.

use std::{path::PathBuf, sync::Arc};

use shared::{
    error::ErrorCode,
    game_recorder::{GameRecorderCmd, RecorderEvent, RecorderEvents, RecordingParams},
    op_state::CanvasOpState,
    protocol::{
        render_cmd::{RenderCmdResp, RenderCommand},
        send_render_with_resp_async, send_render_with_resp_sync,
    },
    vfs::GamePaths,
};
use tracing::warn;

use crate::host_paths::{self, Files, Sandbox};

/// The name the render thread's replies are awaited under.
const OP: &str = "game_recorder";

/// What a call came to.
pub const DONE: u32 = 0;
pub const NOT_SUPPORTED: u32 = 1;
pub const FAILED: u32 = 2;

/// What a recording's share is refused for before the host is asked: the
/// recorder's module answers each in the reference's codes.
pub const NO_RECORDED_VIDEO: &str = "no recorded video";
pub const BGM_NOT_FOUND: &str = "bgm not found";

/// The music a shared recording is set to: a file of the game's, never a URL.
const SHARE_FILES: Files = host_paths::SHARE_GAME_RECORDING;

/// Whether this device can record the game.
pub fn supported(canvas: &CanvasOpState) -> bool {
    send_render_with_resp_sync(canvas, OP, |resp| {
        RenderCommand::GameRecorder(GameRecorderCmd::Supported { resp })
    })
    .unwrap_or(false)
}

/// Start recording into the session's recording file, reporting to `events`;
/// answered once frames are being taken.
pub async fn start(
    canvas: &CanvasOpState,
    paths: Option<&GamePaths>,
    params: RecordingParams,
    events: Arc<RecorderEvents>,
) -> u32 {
    let Some(path) = paths.map(GamePaths::recording_path) else {
        return FAILED;
    };
    if let Some(dir) = path.parent()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        warn!("game recorder: {}: {e}", dir.display());
        return FAILED;
    }
    let started = send_render_with_resp_async(canvas, OP, |resp| {
        RenderCommand::GameRecorder(GameRecorderCmd::Start {
            params,
            path,
            events,
            resp,
        })
    })
    .await;
    match started {
        Ok(()) => DONE,
        Err(e) if e.code == ErrorCode::Unsupported => NOT_SUPPORTED,
        Err(e) => {
            warn!("game recorder: {e}");
            FAILED
        }
    }
}

/// Pause, resume or abort the recording being made.
pub async fn control(
    canvas: &CanvasOpState,
    build: impl FnOnce(RenderCmdResp<()>) -> GameRecorderCmd,
) -> u32 {
    match send_render_with_resp_async(canvas, OP, |resp| RenderCommand::GameRecorder(build(resp)))
        .await
    {
        Ok(()) => DONE,
        Err(e) => {
            warn!("game recorder: {e}");
            FAILED
        }
    }
}

/// End the recording and complete its file: its length in milliseconds, or -1
/// when it failed.
pub async fn stop(canvas: &CanvasOpState) -> f64 {
    match send_render_with_resp_async(canvas, OP, |resp| {
        RenderCommand::GameRecorder(GameRecorderCmd::Stop { resp })
    })
    .await
    {
        Ok(duration_ms) => duration_ms as f64,
        Err(e) => {
            warn!("game recorder: {e}");
            -1.0
        }
    }
}

/// An event of the recording being made.
#[derive(Debug, PartialEq)]
pub enum RecordingEvent {
    Time { ms: f64 },
    Failed { reason: String },
    Closed,
}

impl RecordingEvent {
    /// As content's side reads it: `{"kind": "time", "ms"}`, `{"kind":
    /// "failed", "reason"}` or `{"kind": "closed"}`.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Time { ms } => serde_json::json!({ "kind": "time", "ms": ms }),
            Self::Failed { reason } => serde_json::json!({ "kind": "failed", "reason": reason }),
            Self::Closed => serde_json::json!({ "kind": "closed" }),
        }
    }
}

/// The next event of the recording `events` belongs to, waiting until there
/// is one; `closed` once it has ended, or when none is being made.
pub async fn next_event(events: Option<Arc<RecorderEvents>>) -> RecordingEvent {
    let Some(events) = events else {
        return RecordingEvent::Closed;
    };
    match events.next().await {
        RecorderEvent::Time(ms) => RecordingEvent::Time { ms: ms as f64 },
        RecorderEvent::Failed(reason) => RecordingEvent::Failed { reason },
        RecorderEvent::Closed => RecordingEvent::Closed,
    }
}

/// The game's last finished recording: the file a recording becomes once
/// complete, which the next one replaces only when it completes in turn.
pub fn recorded_video(paths: Option<&GamePaths>) -> Option<PathBuf> {
    paths
        .map(GamePaths::recording_path)
        .filter(|video| video.is_file())
}

/// A recording's share as the host is handed it: content's request with its
/// music resolved through the sandbox and `video`, the engine's own file.
/// Refused in the words above when there is no recording or no such music.
pub async fn share_request(
    sandbox: &Sandbox,
    paths: Option<&GamePaths>,
    request_json: &str,
) -> Result<String, crate::ServiceError> {
    let video =
        recorded_video(paths).ok_or_else(|| crate::ServiceError::generic(NO_RECORDED_VIDEO))?;
    let request = host_paths::export_paths(sandbox, request_json, SHARE_FILES)
        .await
        .map_err(|_| crate::ServiceError::generic(BGM_NOT_FOUND))?;
    let mut request: serde_json::Value = serde_json::from_str(&request)
        .map_err(|e| crate::ServiceError::generic(format!("malformed request: {e}")))?;
    request["video"] = serde_json::Value::String(video.to_string_lossy().into_owned());
    Ok(request.to_string())
}
