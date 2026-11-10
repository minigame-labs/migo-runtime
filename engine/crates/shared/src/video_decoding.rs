//! `createVideoDecoder`'s decoding: a video decoded frame by frame for content to draw.
//!
//! The platform decodes ([`VideoDecoderService`]); this runs the decoding, the same for every execution of content --
//! the embedded runtime's ops and the Performance+ host's service ops both hold a [`VideoDecoders`]. Each decoder
//! content starts gets a thread that pulls frames from the platform's source and hands them to content through a
//! small buffer:
//!
//! - **By presentation time** (`mode: 0`): a frame is released when its time comes, measured from the first frame
//!   (and again from the first frame after a seek), and replaces one content has not taken -- content reading slower
//!   than the video plays sees the video, not a growing backlog.
//! - **As fast as possible** (`mode: 1`, the default): frames are decoded ahead until [`FASTEST_AHEAD`] wait, and
//!   the thread waits for content to take one.
//!
//! What happens is reported to content as the decoder's events -- `start` with the size, `seek`, `stop`, `ended`,
//! and `bufferchange` when frames are there again after content found none -- through one host hook,
//! [`VIDEO_DECODER_EVENT_HOOK`]`(id, event)`, sent on the session's host channel. A frame's pixels are handed over as
//! they were decoded, without a copy.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::host_channel::HostCommandSender;
use crate::protocol::host_cmd::HostCommand;
use crate::services::{FrameTiming, VideoDecoderService};

/// Frames decoded ahead of content when decoding as fast as possible: enough that a frame is ready when content asks,
/// few enough that a 4K video holds about 100 MiB.
pub const FASTEST_AHEAD: usize = 3;

/// The largest frame a decoder takes from a source, in bytes: a 4K frame, with room for a little more. A larger one is
/// refused rather than allocated.
pub const FRAME_BYTES_MAX: usize = 4096 * 4096 * 4;

/// The host hook content hears its decoders' events through, with `[id, event]`.
pub const VIDEO_DECODER_EVENT_HOOK: &str = "_internalOnVideoDecoderEvent";

/// How a decoder hands its frames to content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeMode {
    /// Released when the frame's presentation time comes.
    Pts,
    /// Released as soon as content has room for it.
    Fastest,
}

/// What `start` asks for, read from content's request: `{"source", "mode", "abortVideo"}`.
#[derive(Clone, Debug, PartialEq)]
pub struct StartRequest {
    /// The source as the caller resolved it: a real path or an http(s) URL.
    pub source: String,
    pub mode: DecodeMode,
    /// Whether to decode the video track at all (`abortVideo` is not set).
    pub video: bool,
}

impl StartRequest {
    /// The request content made, its `source` already exported to a real path by the caller.
    pub fn from_request(request: &Value) -> Self {
        Self {
            source: request["source"].as_str().unwrap_or_default().to_owned(),
            mode: if request["mode"].as_u64() == Some(0) {
                DecodeMode::Pts
            } else {
                DecodeMode::Fastest
            },
            video: request["abortVideo"].as_bool() != Some(true),
        }
    }
}

/// A decoded frame, as content receives it.
#[derive(Debug)]
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub timing: FrameTiming,
    /// Straight RGBA8, row-major, top row first.
    pub rgba: Vec<u8>,
}

#[derive(Default)]
struct State {
    frames: VecDeque<DecodedFrame>,
    /// A position to continue from, asked for and not yet reached.
    seek_to: Option<f64>,
    /// Decoding is to end: `stop` or `remove`.
    halt: bool,
    /// Whether the decoder was removed, which ends it without a `stop` event.
    removed: bool,
    /// Content asked for a frame and there was none, so the next one is news.
    starved: bool,
}

/// What a decode thread and the calls share for one decoder.
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

/// Content's decoders, by id, for as long as the session lives. Dropping it -- the session ending -- ends every decode
/// thread.
#[derive(Default)]
pub struct VideoDecoders {
    runs: HashMap<u32, Arc<Shared>>,
}

impl Drop for VideoDecoders {
    fn drop(&mut self) {
        for shared in self.runs.values() {
            shared.halt(true);
        }
    }
}

impl VideoDecoders {
    /// Start decoding `request` as decoder `id`, replacing whatever it was decoding. The source opens on the decode
    /// thread; content hears `start` -- or `error` -- from there, on `host`. `Err` with the reason when no thread could
    /// be made.
    pub fn start(
        &mut self,
        id: u32,
        service: Arc<dyn VideoDecoderService>,
        request: StartRequest,
        host: HostCommandSender,
    ) -> Result<(), String> {
        let events = Events { host, id };
        let shared = Arc::new(Shared::default());
        if let Some(previous) = self.runs.insert(id, Arc::clone(&shared)) {
            previous.halt(true);
        }
        std::thread::Builder::new()
            .name(format!("migo-video-decoder-{id}"))
            .spawn(move || decode(service.as_ref(), &request, &shared, &events))
            .map(drop)
            .map_err(|error| error.to_string())
    }

    /// The oldest frame content has not taken -- or, decoding by presentation time, the latest one due. `None` when
    /// there is none, which makes the next frame news (`bufferchange`).
    pub fn take_frame(&self, id: u32) -> Option<DecodedFrame> {
        let shared = self.runs.get(&id)?;
        let mut run = shared.lock();
        let Some(frame) = run.frames.pop_front() else {
            run.starved = true;
            return None;
        };
        drop(run);
        // Room for the next one, for a thread decoding ahead.
        shared.changed.notify_all();
        Some(frame)
    }

    /// Continue decoder `id` from `position_ms`; content hears `seek` when it has. False when it is not decoding.
    pub fn seek(&self, id: u32, position_ms: f64) -> bool {
        let Some(shared) = self.runs.get(&id) else {
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

    /// End decoder `id`'s decoding; content hears `stop` when it has. False when it was not decoding.
    pub fn stop(&mut self, id: u32) -> bool {
        match self.runs.remove(&id) {
            Some(shared) => {
                shared.halt(false);
                true
            }
            None => false,
        }
    }

    /// End decoder `id` for good, without an event.
    pub fn remove(&mut self, id: u32) {
        if let Some(shared) = self.runs.remove(&id) {
            shared.halt(true);
        }
    }
}

/// The decoder's events, to content.
#[derive(Clone)]
struct Events {
    host: HostCommandSender,
    id: u32,
}

impl Events {
    fn send(&self, event: Value) {
        let args = json!([self.id, event]).to_string();
        let command = HostCommand::InvokeHostHook {
            hook: VIDEO_DECODER_EVENT_HOOK,
            args_json: std::borrow::Cow::Owned(args),
        };
        if let Err(error) = self.host.try_send_reliable(None, command) {
            tracing::debug!("video decoder {}: event not delivered: {error:?}", self.id);
        }
    }

    fn fail(&self, message: &str) {
        self.send(json!({ "event": "error", "errMsg": message }));
    }
}

/// One decoder's thread: open, then decode until halted, following seeks.
fn decode(
    service: &dyn VideoDecoderService,
    request: &StartRequest,
    shared: &Shared,
    events: &Events,
) {
    let mut opened = match service.open(&request.source) {
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
    let mut idle = !request.video;
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
            idle = !request.video;
            events.send(json!({ "event": "seek" }));
            continue;
        }

        let mut rgba = vec![0u8; bytes];
        match opened.next_frame(&mut rgba) {
            Ok(Some(timing)) => {
                let frame = DecodedFrame {
                    width,
                    height,
                    timing,
                    rgba,
                };
                if !release(shared, frame, request.mode, &mut clock, events) {
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

/// Hand `frame` to content by `mode`'s rule. False when the decoder is halted; a seek asked for meanwhile drops the
/// frame and returns true, so the caller takes it next.
fn release(
    shared: &Shared,
    frame: DecodedFrame,
    mode: DecodeMode,
    clock: &mut Option<(Instant, f64)>,
    events: &Events,
) -> bool {
    let mut state = shared.lock();
    match mode {
        DecodeMode::Pts => {
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
        DecodeMode::Fastest => {
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

    fn frame(index: u8, pts: f64) -> DecodedFrame {
        DecodedFrame {
            width: 2,
            height: 1,
            timing: FrameTiming {
                pts_ms: pts,
                dts_ms: pts,
            },
            rgba: vec![index; 8],
        }
    }

    fn events() -> (Events, crate::host_channel::HostCommandReceiver) {
        let (host_tx, _critical, host_rx) = crate::host_channel::channel(64);
        (
            Events {
                host: host_tx,
                id: 7,
            },
            host_rx,
        )
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
                DecodeMode::Fastest,
                &mut clock,
                &events
            ));
        }
        let waiting = {
            let shared = Arc::clone(&shared);
            let events = events.clone();
            std::thread::spawn(move || {
                let mut clock = None;
                release(
                    &shared,
                    frame(9, 0.0),
                    DecodeMode::Fastest,
                    &mut clock,
                    &events,
                )
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
            DecodeMode::Pts,
            &mut clock,
            &events
        ));
        assert!(release(
            &shared,
            frame(1, 1040.0),
            DecodeMode::Pts,
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
            DecodeMode::Pts,
            &mut clock,
            &events
        ));
        let waiting = {
            let shared = Arc::clone(&shared);
            let events = events.clone();
            let mut clock = clock;
            std::thread::spawn(move || {
                release(
                    &shared,
                    frame(1, 60_000.0),
                    DecodeMode::Pts,
                    &mut clock,
                    &events,
                )
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
            DecodeMode::Fastest,
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
            DecodeMode::Fastest,
            &mut clock,
            &events
        ));
        assert!(rx.try_recv().is_err(), "no news before content asked");
        shared.lock().frames.clear();
        shared.lock().starved = true;
        assert!(release(
            &shared,
            frame(1, 0.0),
            DecodeMode::Fastest,
            &mut clock,
            &events
        ));
        match rx.try_recv() {
            Ok(HostCommand::InvokeHostHook { hook, args_json }) => {
                assert_eq!(hook, VIDEO_DECODER_EVENT_HOOK);
                assert_eq!(args_json, r#"[7,{"event":"bufferchange"}]"#);
            }
            other => panic!("{other:?}"),
        }
    }

    /// A source of `count` frames of 2x1 pixels, 40 ms apart, each filled with its index.
    struct Counted {
        count: u8,
        next: u8,
    }

    impl crate::services::VideoFrameSource for Counted {
        fn size(&self) -> (u32, u32) {
            (2, 1)
        }

        fn next_frame(
            &mut self,
            rgba: &mut [u8],
        ) -> Result<Option<FrameTiming>, crate::protocol::error::ServiceError> {
            if self.next == self.count {
                return Ok(None);
            }
            rgba.fill(self.next);
            let pts = f64::from(self.next) * 40.0;
            self.next += 1;
            Ok(Some(FrameTiming {
                pts_ms: pts,
                dts_ms: pts,
            }))
        }

        fn seek(&mut self, position_ms: f64) -> Result<(), crate::protocol::error::ServiceError> {
            self.next = (position_ms / 40.0) as u8;
            Ok(())
        }
    }

    struct CountedService;

    impl VideoDecoderService for CountedService {
        fn open(
            &self,
            source: &str,
        ) -> Result<Box<dyn crate::services::VideoFrameSource>, crate::protocol::error::ServiceError>
        {
            if source == "missing" {
                return Err(crate::protocol::error::ServiceError::system(
                    "no such video",
                ));
            }
            Ok(Box::new(Counted { count: 5, next: 0 }))
        }
    }

    /// The next event decoder 7 sends, within a second.
    fn next_event(rx: &mut crate::host_channel::HostCommandReceiver) -> Value {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match rx.try_recv() {
                Ok(HostCommand::InvokeHostHook { hook, args_json }) => {
                    assert_eq!(hook, VIDEO_DECODER_EVENT_HOOK);
                    let args: Value = serde_json::from_str(&args_json).unwrap();
                    assert_eq!(args[0], 7);
                    // Frames there again after a take found none: news, in any order.
                    if args[1]["event"] != "bufferchange" {
                        return args[1].clone();
                    }
                }
                Ok(other) => panic!("{other:?}"),
                Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
                Err(error) => panic!("no event: {error:?}"),
            }
        }
    }

    fn take_within(decoders: &VideoDecoders, id: u32) -> DecodedFrame {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(frame) = decoders.take_frame(id) {
                return frame;
            }
            assert!(Instant::now() < deadline, "no frame");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn a_decoder_starts_hands_over_its_frames_seeks_ends_and_stops() {
        let (host, _critical, mut rx) = crate::host_channel::channel(64);
        let mut decoders = VideoDecoders::default();
        let request = StartRequest::from_request(&json!({ "source": "clip.mp4" }));
        assert_eq!(request.mode, DecodeMode::Fastest);
        decoders
            .start(7, Arc::new(CountedService), request, host)
            .unwrap();
        assert_eq!(
            next_event(&mut rx),
            json!({ "event": "start", "width": 2, "height": 1 })
        );

        let first = take_within(&decoders, 7);
        assert_eq!(
            (first.width, first.height, first.rgba.as_slice()),
            (2, 1, &[0u8; 8][..])
        );
        assert_eq!(take_within(&decoders, 7).timing.pts_ms, 40.0);

        assert!(decoders.seek(7, 160.0));
        assert_eq!(next_event(&mut rx), json!({ "event": "seek" }));
        let after = take_within(&decoders, 7);
        assert_eq!(after.rgba[0], 4, "the frame at 160 ms");
        assert_eq!(next_event(&mut rx), json!({ "event": "ended" }));

        assert!(decoders.stop(7));
        assert_eq!(next_event(&mut rx), json!({ "event": "stop" }));
        assert!(!decoders.stop(7), "stopped already");
        assert!(!decoders.seek(7, 0.0));
        assert!(decoders.take_frame(7).is_none());
    }

    #[test]
    fn a_source_that_does_not_open_is_an_error_and_a_removed_decoder_says_nothing() {
        let (host, _critical, mut rx) = crate::host_channel::channel(64);
        let mut decoders = VideoDecoders::default();
        let missing = StartRequest::from_request(&json!({ "source": "missing", "mode": 0 }));
        assert_eq!(missing.mode, DecodeMode::Pts);
        decoders
            .start(7, Arc::new(CountedService), missing, host.clone())
            .unwrap();
        assert_eq!(
            next_event(&mut rx),
            json!({ "event": "error", "errMsg": "no such video" })
        );

        let quiet =
            StartRequest::from_request(&json!({ "source": "clip.mp4", "abortVideo": true }));
        assert!(!quiet.video);
        decoders
            .start(7, Arc::new(CountedService), quiet, host)
            .unwrap();
        assert_eq!(
            next_event(&mut rx),
            json!({ "event": "start", "width": 2, "height": 1 })
        );
        assert_eq!(
            next_event(&mut rx),
            json!({ "event": "ended" }),
            "no video track decoded"
        );
        decoders.remove(7);
        std::thread::sleep(Duration::from_millis(50));
        assert!(rx.try_recv().is_err(), "removal sends no stop");
    }
}
