//! `createVideoDecoder`: a video decoded frame by frame for content to draw.
//!
//! The platform decodes ([`VideoDecoderService`]); this runs the decoding. Each
//! decoder content starts gets a thread that pulls frames from the platform's
//! source and hands them to content through a small buffer:
//!
//! - **By presentation time** (`mode: 0`): a frame is released when its time
//!   comes, measured from the first frame (and again from the first frame after
//!   a seek), and replaces one content has not taken -- content reading slower
//!   than the video plays sees the video, not a growing backlog.
//! - **As fast as possible** (`mode: 1`, the default): frames are decoded ahead
//!   until [`FASTEST_AHEAD`] wait, and the thread waits for content to take one.
//!
//! What happens is reported to content as the decoder's events -- `start` with
//! the size, `seek`, `stop`, `ended`, and `bufferchange` when frames are there
//! again after content found none -- through one host hook,
//! `_internalOnVideoDecoderEvent(id, event)`. A frame's pixels are handed over
//! as they were decoded: the buffer becomes the `ArrayBuffer` content receives,
//! without a copy.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use deno_core::serde_json::{self, Value, json};
use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use shared::op_state::{HostOpState, HostTx};
use shared::protocol::host_cmd::HostCommand;
use shared::services::{FrameTiming, VideoDecoderService};

use crate::file::host_paths::export_paths_for_session;

/// Frames decoded ahead of content when decoding as fast as possible: enough
/// that a frame is ready when content asks, few enough that a 4K video holds
/// about 100 MiB.
const FASTEST_AHEAD: usize = 3;

/// The largest frame the engine takes from a source, in bytes: a 4K frame,
/// with room for a little more. A larger one is refused rather than allocated.
const FRAME_BYTES_MAX: usize = 4096 * 4096 * 4;

const HOOK: &str = "_internalOnVideoDecoderEvent";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Released when the frame's presentation time comes.
    Pts,
    /// Released as soon as content has room for it.
    Fastest,
}

struct Frame {
    width: u32,
    height: u32,
    timing: FrameTiming,
    rgba: Vec<u8>,
}

#[derive(Default)]
struct State {
    frames: VecDeque<Frame>,
    /// A position to continue from, asked for and not yet reached.
    seek_to: Option<f64>,
    /// Decoding is to end: `stop` or `remove`.
    halt: bool,
    /// Whether the decoder was removed, which ends it without a `stop` event.
    removed: bool,
    /// Content asked for a frame and there was none, so the next one is news.
    starved: bool,
}

/// What a decode thread and the ops share for one decoder.
#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn halt(&self, removed: bool) {
        let mut state = self.lock();
        state.halt = true;
        state.removed |= removed;
        state.frames.clear();
        drop(state);
        self.changed.notify_all();
    }
}

/// Content's decoders, by id, for as long as the runtime lives. Dropping it --
/// the runtime ending -- ends every decode thread.
#[derive(Default)]
pub(crate) struct VideoDecoders {
    runs: HashMap<u32, Arc<Shared>>,
}

impl Drop for VideoDecoders {
    fn drop(&mut self) {
        for shared in self.runs.values() {
            shared.halt(true);
        }
    }
}

/// The decoder's events, to content.
#[derive(Clone)]
struct Events {
    host_tx: HostTx,
    id: u32,
}

impl Events {
    fn send(&self, event: Value) {
        let args = json!([self.id, event]).to_string();
        let command = HostCommand::InvokeHostHook {
            hook: HOOK,
            args_json: Cow::Owned(args),
        };
        if let Err(error) = self.host_tx.try_send_reliable(None, command) {
            tracing::debug!("video decoder {}: event not delivered: {error:?}", self.id);
        }
    }

    fn fail(&self, message: &str) {
        self.send(json!({ "event": "error", "errMsg": message }));
    }
}

/// The runtime's decoders, created with the first one.
fn decoders(state: &mut OpState) -> &mut VideoDecoders {
    if !state.has::<VideoDecoders>() {
        state.put(VideoDecoders::default());
    }
    state.borrow_mut::<VideoDecoders>()
}

/// Decoder `id`'s run, if it is decoding.
fn run(state: &mut OpState, id: u32) -> Option<Arc<Shared>> {
    decoders(state).runs.get(&id).cloned()
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
    let source = request["source"].as_str().unwrap_or_default().to_owned();
    let mode = if request["mode"].as_u64() == Some(0) {
        Mode::Pts
    } else {
        Mode::Fastest
    };
    let video = request["abortVideo"].as_bool() != Some(true);

    let mut st = state.borrow_mut();
    let events = Events {
        host_tx: st.borrow::<HostOpState>().host_tx.clone(),
        id,
    };
    let shared = Arc::new(Shared::default());
    if let Some(previous) = decoders(&mut st).runs.insert(id, Arc::clone(&shared)) {
        previous.halt(true);
    }
    std::thread::Builder::new()
        .name(format!("migo-video-decoder-{id}"))
        .spawn(move || decode(service.as_ref(), &source, mode, video, &shared, &events))
        .map(drop)
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
    let Some(shared) = run(state, id) else {
        return Vec::new();
    };
    let mut run = shared.lock();
    let Some(frame) = run.frames.pop_front() else {
        run.starved = true;
        return Vec::new();
    };
    drop(run);
    // Room for the next one, for a thread decoding ahead.
    shared.changed.notify_all();
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
    let Some(shared) = run(state, id) else {
        return false;
    };
    let mut run = shared.lock();
    if run.halt {
        return false;
    }
    run.seek_to = Some(position_ms.max(0.0));
    drop(run);
    shared.changed.notify_all();
    true
}

/// End decoder `id`'s decoding; content hears `stop` when it has. False when
/// it was not decoding.
#[op2(fast)]
pub fn op_video_decoder_stop(state: &mut OpState, #[smi] id: u32) -> bool {
    match decoders(state).runs.remove(&id) {
        Some(shared) => {
            shared.halt(false);
            true
        }
        None => false,
    }
}

/// End decoder `id` for good, without an event.
#[op2(fast)]
pub fn op_video_decoder_remove(state: &mut OpState, #[smi] id: u32) {
    if let Some(shared) = decoders(state).runs.remove(&id) {
        shared.halt(true);
    }
}

/// One decoder's thread: open, then decode until halted, following seeks.
fn decode(
    service: &dyn VideoDecoderService,
    source: &str,
    mode: Mode,
    video: bool,
    shared: &Shared,
    events: &Events,
) {
    let mut opened = match service.open(source) {
        Ok(opened) => opened,
        Err(error) => {
            events.fail(&error.message);
            return;
        }
    };
    let (width, height) = opened.size();
    let bytes = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|bytes| *bytes > 0 && *bytes <= FRAME_BYTES_MAX);
    let Some(bytes) = bytes else {
        events.fail(&format!(
            "a {width}x{height} video is not one this decoder holds"
        ));
        return;
    };
    events.send(json!({ "event": "start", "width": width, "height": height }));

    // Where presentation time 0 of the current run falls on the clock.
    let mut clock: Option<(Instant, f64)> = None;
    // Whether the stream ended, or failed: then the thread waits for a seek.
    let mut idle = !video;
    if idle {
        events.send(json!({ "event": "ended" }));
    }
    loop {
        let seek = {
            let mut state = shared.lock();
            while idle && !state.halt && state.seek_to.is_none() {
                state = shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            if state.halt {
                break;
            }
            state.seek_to.take()
        };
        if let Some(position) = seek {
            if let Err(error) = opened.seek(position) {
                events.fail(&error.message);
                idle = true;
                continue;
            }
            shared.lock().frames.clear();
            clock = None;
            idle = !video;
            events.send(json!({ "event": "seek" }));
            continue;
        }

        let mut rgba = vec![0u8; bytes];
        match opened.next_frame(&mut rgba) {
            Ok(Some(timing)) => {
                let frame = Frame {
                    width,
                    height,
                    timing,
                    rgba,
                };
                if !release(shared, frame, mode, &mut clock, events) {
                    break;
                }
            }
            Ok(None) => {
                idle = true;
                events.send(json!({ "event": "ended" }));
            }
            Err(error) => {
                idle = true;
                events.fail(&error.message);
            }
        }
    }
    drop(opened);
    if !shared.lock().removed {
        events.send(json!({ "event": "stop" }));
    }
}

/// Hand `frame` to content by `mode`'s rule. False when the decoder is halted;
/// a seek asked for meanwhile drops the frame and returns true, so the caller
/// takes it next.
fn release(
    shared: &Shared,
    frame: Frame,
    mode: Mode,
    clock: &mut Option<(Instant, f64)>,
    events: &Events,
) -> bool {
    let mut state = shared.lock();
    match mode {
        Mode::Pts => {
            let (start, origin) = *clock.get_or_insert((Instant::now(), frame.timing.pts_ms));
            let offset = (frame.timing.pts_ms - origin).max(0.0);
            let due = start + Duration::from_secs_f64(offset / 1000.0);
            loop {
                if state.halt || state.seek_to.is_some() {
                    return !state.halt;
                }
                let now = Instant::now();
                if now >= due {
                    break;
                }
                state = shared
                    .changed
                    .wait_timeout(state, due - now)
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .0;
            }
            // Newest wins: a frame content did not take is no longer the one to show.
            state.frames.clear();
        }
        Mode::Fastest => {
            while state.frames.len() >= FASTEST_AHEAD {
                if state.halt || state.seek_to.is_some() {
                    return !state.halt;
                }
                state = shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            if state.halt || state.seek_to.is_some() {
                return !state.halt;
            }
        }
    }
    state.frames.push_back(frame);
    let news = std::mem::take(&mut state.starved);
    drop(state);
    if news {
        events.send(json!({ "event": "bufferchange" }));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(index: u8, pts: f64) -> Frame {
        Frame {
            width: 2,
            height: 1,
            timing: FrameTiming {
                pts_ms: pts,
                dts_ms: pts,
            },
            rgba: vec![index; 8],
        }
    }

    fn events() -> (Events, shared::host_channel::HostCommandReceiver) {
        let (host_tx, _critical, host_rx) = shared::host_channel::channel(64);
        (Events { host_tx, id: 7 }, host_rx)
    }

    #[test]
    fn decoding_ahead_stops_at_the_bound_and_resumes_when_content_takes_one() {
        let shared = Arc::new(Shared::default());
        let (events, _rx) = events();
        let mut clock = None;
        for i in 0..FASTEST_AHEAD as u8 {
            assert!(release(
                &shared,
                frame(i, 0.0),
                Mode::Fastest,
                &mut clock,
                &events
            ));
        }
        let waiting = {
            let shared = Arc::clone(&shared);
            let events = events.clone();
            std::thread::spawn(move || {
                let mut clock = None;
                release(&shared, frame(9, 0.0), Mode::Fastest, &mut clock, &events)
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            shared.lock().frames.len(),
            FASTEST_AHEAD,
            "the fourth waits"
        );
        shared.lock().frames.pop_front();
        shared.changed.notify_all();
        assert!(waiting.join().unwrap());
        let order: Vec<u8> = shared.lock().frames.iter().map(|f| f.rgba[0]).collect();
        assert_eq!(order, [1, 2, 9]);
    }

    #[test]
    fn by_presentation_time_the_newest_due_frame_replaces_one_not_taken() {
        let shared = Arc::new(Shared::default());
        let (events, _rx) = events();
        let mut clock = None;
        let started = Instant::now();
        assert!(release(
            &shared,
            frame(0, 1000.0),
            Mode::Pts,
            &mut clock,
            &events
        ));
        assert!(release(
            &shared,
            frame(1, 1040.0),
            Mode::Pts,
            &mut clock,
            &events
        ));
        // Released no earlier than its time, measured from the first frame.
        assert!(started.elapsed() >= Duration::from_millis(40));
        let held: Vec<u8> = shared.lock().frames.iter().map(|f| f.rgba[0]).collect();
        assert_eq!(held, [1]);
    }

    #[test]
    fn a_halt_or_a_seek_ends_the_wait_and_drops_the_frame() {
        let shared = Arc::new(Shared::default());
        let (events, _rx) = events();
        let mut clock = None;
        assert!(release(
            &shared,
            frame(0, 0.0),
            Mode::Pts,
            &mut clock,
            &events
        ));
        let waiting = {
            let shared = Arc::clone(&shared);
            let events = events.clone();
            let mut clock = clock;
            std::thread::spawn(move || {
                release(&shared, frame(1, 60_000.0), Mode::Pts, &mut clock, &events)
            })
        };
        std::thread::sleep(Duration::from_millis(20));
        shared.lock().seek_to = Some(0.0);
        shared.changed.notify_all();
        assert!(waiting.join().unwrap(), "a seek is not a halt");
        assert_eq!(
            shared.lock().frames.len(),
            1,
            "the frame not yet due was dropped"
        );
        shared.halt(false);
        let mut clock = None;
        assert!(!release(
            &shared,
            frame(2, 0.0),
            Mode::Fastest,
            &mut clock,
            &events
        ));
    }

    #[test]
    fn frames_after_content_found_none_are_news_and_others_are_not() {
        let shared = Arc::new(Shared::default());
        let (events, mut rx) = events();
        let mut clock = None;
        assert!(release(
            &shared,
            frame(0, 0.0),
            Mode::Fastest,
            &mut clock,
            &events
        ));
        assert!(rx.try_recv().is_err(), "no news before content asked");
        shared.lock().frames.clear();
        shared.lock().starved = true;
        assert!(release(
            &shared,
            frame(1, 0.0),
            Mode::Fastest,
            &mut clock,
            &events
        ));
        match rx.try_recv() {
            Ok(HostCommand::InvokeHostHook { hook, args_json }) => {
                assert_eq!(hook, HOOK);
                assert_eq!(args_json, r#"[7,{"event":"bufferchange"}]"#);
            }
            other => panic!("{other:?}"),
        }
    }
}
