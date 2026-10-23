//! `getGameRecorder`'s ops: a recording asked of the render thread, and the
//! events it reports on its own -- `migo_services::game_recorder`'s requests,
//! which the Performance+ host makes for its producer too.

use std::{cell::RefCell, rc::Rc, sync::Arc};

use deno_core::{OpState, op2, serde_json};
use migo_services::game_recorder::{self as recorder, FAILED};
use shared::{
    game_recorder::{GameRecorderCmd, RecorderEvents, RecordingParams},
    op_state::{CanvasOpState, HostOpState},
};

/// The events of the recording being made, for the op waiting on them.
#[derive(Default)]
struct RecordingEvents(Option<Arc<RecorderEvents>>);

fn canvas(state: &Rc<RefCell<OpState>>) -> Option<CanvasOpState> {
    state.borrow().try_borrow::<CanvasOpState>().cloned()
}

/// Whether this device can record the game.
#[op2(fast)]
pub fn op_game_recorder_supported(state: &mut OpState) -> bool {
    state
        .try_borrow::<CanvasOpState>()
        .is_some_and(recorder::supported)
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
    let paths = state.borrow().borrow::<HostOpState>().game_paths.clone();
    let Some(ctx) = canvas(&state) else {
        return FAILED;
    };
    let events = Arc::new(RecorderEvents::default());
    state
        .borrow_mut()
        .put(RecordingEvents(Some(Arc::clone(&events))));
    recorder::start(&ctx, paths.as_deref(), params, events).await
}

async fn control(
    state: Rc<RefCell<OpState>>,
    build: impl FnOnce(shared::protocol::render_cmd::RenderCmdResp<()>) -> GameRecorderCmd,
) -> u32 {
    match canvas(&state) {
        Some(ctx) => recorder::control(&ctx, build).await,
        None => FAILED,
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
    match canvas(&state) {
        Some(ctx) => recorder::stop(&ctx).await,
        None => -1.0,
    }
}

/// The next event of the recording being made, waiting until there is one;
/// `closed` once it has ended, or when none is being made.
#[op2(async(lazy), fast)]
#[serde]
pub async fn op_game_recorder_next_event(state: Rc<RefCell<OpState>>) -> serde_json::Value {
    let events = state
        .borrow()
        .try_borrow::<RecordingEvents>()
        .and_then(|events| events.0.clone());
    recorder::next_event(events).await.to_json()
}
