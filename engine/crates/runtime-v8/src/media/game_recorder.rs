//! `getGameRecorder`'s ops: a recording asked of the render thread, and the
//! events it reports on its own. Content's calls are judged in
//! `05_game_recorder.js`, which owns the reference's error codes; these answer
//! with what happened -- started, not supported, failed.

use std::{cell::RefCell, rc::Rc, sync::Arc};

use deno_core::{OpState, op2};
use serde::Serialize;
use shared::{
    error::ErrorCode,
    game_recorder::{GameRecorderCmd, RecorderEvent, RecorderEvents, RecordingParams},
    op_state::{CanvasOpState, HostOpState},
    protocol::{
        render_cmd::RenderCommand, send_render_with_resp_async, send_render_with_resp_sync,
    },
};
use tracing::warn;

/// The name the render thread's replies are awaited under.
const OP: &str = "game_recorder";

/// What a call came to.
const DONE: u32 = 0;
const NOT_SUPPORTED: u32 = 1;
const FAILED: u32 = 2;

/// The events of the recording being made, for the op waiting on them.
#[derive(Default)]
struct RecordingEvents(Option<Arc<RecorderEvents>>);

fn canvas(state: &Rc<RefCell<OpState>>) -> Option<CanvasOpState> {
    state.borrow().try_borrow::<CanvasOpState>().cloned()
}

/// Whether this device can record the game.
#[op2(fast)]
pub fn op_game_recorder_supported(state: &mut OpState) -> bool {
    let Some(ctx) = state.try_borrow::<CanvasOpState>() else {
        return false;
    };
    send_render_with_resp_sync(ctx, OP, |resp| {
        RenderCommand::GameRecorder(GameRecorderCmd::Supported { resp })
    })
    .unwrap_or(false)
}

/// Start recording with options content's side judged; answered once frames
/// are being taken.
#[op2(async(lazy), fast)]
pub async fn op_game_recorder_start(
    state: Rc<RefCell<OpState>>,
    #[smi] fps: u32,
    #[smi] bitrate_kbps: u32,
    #[smi] gop: u32,
    duration_ms: f64,
) -> u32 {
    let params = RecordingParams {
        fps,
        bitrate_kbps,
        gop,
        duration_ms: duration_ms as u64,
    };
    let path = {
        let state = state.borrow();
        state
            .borrow::<HostOpState>()
            .game_paths
            .as_ref()
            .map(|paths| paths.recording_path())
    };
    let (Some(ctx), Some(path)) = (canvas(&state), path) else {
        return FAILED;
    };
    if let Some(dir) = path.parent()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        warn!("game recorder: {}: {e}", dir.display());
        return FAILED;
    }
    let events = Arc::new(RecorderEvents::default());
    state
        .borrow_mut()
        .put(RecordingEvents(Some(Arc::clone(&events))));
    let started = send_render_with_resp_async(&ctx, OP, |resp| {
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

async fn control(
    state: Rc<RefCell<OpState>>,
    build: impl FnOnce(shared::protocol::render_cmd::RenderCmdResp<()>) -> GameRecorderCmd,
) -> u32 {
    let Some(ctx) = canvas(&state) else {
        return FAILED;
    };
    match send_render_with_resp_async(&ctx, OP, |resp| RenderCommand::GameRecorder(build(resp)))
        .await
    {
        Ok(()) => DONE,
        Err(e) => {
            warn!("game recorder: {e}");
            FAILED
        }
    }
}

#[op2(async(lazy), fast)]
pub async fn op_game_recorder_pause(state: Rc<RefCell<OpState>>) -> u32 {
    control(state, |resp| GameRecorderCmd::Pause { resp }).await
}

#[op2(async(lazy), fast)]
pub async fn op_game_recorder_resume(state: Rc<RefCell<OpState>>) -> u32 {
    control(state, |resp| GameRecorderCmd::Resume { resp }).await
}

#[op2(async(lazy), fast)]
pub async fn op_game_recorder_abort(state: Rc<RefCell<OpState>>) -> u32 {
    control(state, |resp| GameRecorderCmd::Abort { resp }).await
}

/// End the recording and complete its file: its length in milliseconds, or -1
/// when it failed.
#[op2(async(lazy), fast)]
pub async fn op_game_recorder_stop(state: Rc<RefCell<OpState>>) -> f64 {
    let Some(ctx) = canvas(&state) else {
        return -1.0;
    };
    match send_render_with_resp_async(&ctx, OP, |resp| {
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

/// An event of the recording being made, as content's side reads it.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RecordingEventJs {
    Time { ms: f64 },
    Failed { reason: String },
    Closed,
}

/// The next event of the recording being made, waiting until there is one;
/// `closed` once it has ended, or when none is being made.
#[op2(async(lazy), fast)]
#[serde]
pub async fn op_game_recorder_next_event(state: Rc<RefCell<OpState>>) -> RecordingEventJs {
    let events = state
        .borrow()
        .try_borrow::<RecordingEvents>()
        .and_then(|events| events.0.clone());
    let Some(events) = events else {
        return RecordingEventJs::Closed;
    };
    match events.next().await {
        RecorderEvent::Time(ms) => RecordingEventJs::Time { ms: ms as f64 },
        RecorderEvent::Failed(reason) => RecordingEventJs::Failed { reason },
        RecorderEvent::Closed => RecordingEventJs::Closed,
    }
}
