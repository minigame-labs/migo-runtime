//! `createVideoDecoder`'s ops: content's decoders, decoded by
//! [`shared::video_decoding`] -- the decoding the Performance+ host runs for
//! its service ops too -- with the platform's [`VideoDecoderService`]. A
//! frame's pixels become the `ArrayBuffer` content receives, without a copy.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use deno_core::serde_json::{self, Value};
use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use shared::op_state::HostOpState;
use shared::services::VideoDecoderService;
use shared::video_decoding::{StartRequest, VideoDecoders};

use crate::file::host_paths::export_paths_for_session;

/// The runtime's decoders, created with the first one. Dropped with the
/// runtime, which ends every decode thread.
fn decoders(state: &mut OpState) -> &mut VideoDecoders {
    if !state.has::<VideoDecoders>() {
        state.put(VideoDecoders::default());
    }
    state.borrow_mut::<VideoDecoders>()
}

fn service(state: &OpState) -> Option<Arc<dyn VideoDecoderService>> {
    state
        .borrow::<HostOpState>()
        .device_services
        .as_ref()
        .and_then(|services| services.video_decoder())
}

/// Whether this platform decodes video at all.
#[op2(fast)]
pub fn op_video_decoder_available(state: &mut OpState) -> bool {
    service(state).is_some()
}

/// Start decoding `{"source", "mode", "abortVideo"}` as decoder `id`,
/// replacing whatever it was decoding. The source opens on the decode thread;
/// content hears `start` -- or `error` -- from there.
#[op2]
pub async fn op_video_decoder_start(
    state: Rc<RefCell<OpState>>,
    #[smi] id: u32,
    #[string] request_json: String,
) -> Result<(), JsErrorBox> {
    let service =
        service(&state.borrow()).ok_or_else(|| JsErrorBox::generic("start:fail not supported"))?;
    let mut request: Value = serde_json::from_str(&request_json)
        .map_err(|error| JsErrorBox::generic(format!("start:fail malformed request: {error}")))?;
    // Held for the session: the decoder reads the file for as long as it runs.
    export_paths_for_session(
        &state,
        &mut request,
        migo_services::host_paths::VIDEO_DECODER_START.fields,
        migo_services::host_paths::VIDEO_DECODER_START.remote,
    )
    .await?;
    let mut st = state.borrow_mut();
    let host = st.borrow::<HostOpState>().host_tx.clone();
    decoders(&mut st)
        .start(id, service, StartRequest::from_request(&request), host)
        .map_err(|error| JsErrorBox::generic(format!("start:fail {error}")))
}

/// The oldest frame content has not taken -- or, decoding by presentation
/// time, the latest one due -- with its size and times written to `meta`
/// (`[width, height, pts, dts]`). Empty when there is none.
#[op2]
#[buffer]
pub fn op_video_decoder_take_frame(
    state: &mut OpState,
    #[smi] id: u32,
    #[buffer] meta: &mut [f64],
) -> Vec<u8> {
    let Some(frame) = decoders(state).take_frame(id) else {
        return Vec::new();
    };
    if let [width, height, pts, dts, ..] = meta {
        *width = f64::from(frame.width);
        *height = f64::from(frame.height);
        *pts = frame.timing.pts_ms;
        *dts = frame.timing.dts_ms;
    }
    frame.rgba
}

/// Continue decoder `id` from `position_ms`; content hears `seek` when it has.
/// False when it is not decoding.
#[op2(fast)]
pub fn op_video_decoder_seek(state: &mut OpState, #[smi] id: u32, position_ms: f64) -> bool {
    decoders(state).seek(id, position_ms)
}

/// End decoder `id`'s decoding; content hears `stop` when it has. False when
/// it was not decoding.
#[op2(fast)]
pub fn op_video_decoder_stop(state: &mut OpState, #[smi] id: u32) -> bool {
    decoders(state).stop(id)
}

/// End decoder `id` for good, without an event.
#[op2(fast)]
pub fn op_video_decoder_remove(state: &mut OpState, #[smi] id: u32) {
    decoders(state).remove(id);
}
