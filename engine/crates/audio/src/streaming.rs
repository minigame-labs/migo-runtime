//! Streaming audio download and decode for edge-download-edge-play
//!
//! Downloads audio from URL in chunks and decodes progressively.
//! MP3, PCM WAVE, Ogg/Vorbis and AAC-LC ADTS use bounded progressive decoders.
//!
//! Uses a shared tokio runtime for all download tasks to avoid per-stream overhead.

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use shared::error::{EngineError, EngineResult, ErrorCode};
use tokio::sync::{Notify, mpsc};

#[cfg(test)]
use crate::off_worker::OffWorker;
#[cfg(test)]
use crate::streaming_mp3::Mp3StreamDecoder;

/// Keep only a small number of decoded chunks ahead of the audio thread.
/// When the app is backgrounded and stops polling, async sends apply
/// backpressure to both decoding and the network response body.
pub(crate) const STREAM_CHANNEL_CAPACITY: usize = 4;

/// Bound a server that accepts a connection but never produces headers.
pub(crate) const RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(30);
/// Per-chunk inactivity bound. This deliberately does not cap track duration.
pub(crate) const BODY_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// The engine's HTTP client, as streaming holds it.
pub type HttpClient = migo_services::network::http::HttpClient;

/// Per-host factory captured by `AudioService`. Building remains lazy so local
/// audio and hosts that never stream do not allocate a connection pool.
pub type StreamingHttpClientFactory =
    Arc<dyn Fn() -> EngineResult<HttpClient> + Send + Sync + 'static>;

/// One lazily-built client for one host audio thread. Cloning it shares its
/// connection pool, and the clone is the only value moved into each task.
pub(crate) struct LazyStreamingClient {
    factory: StreamingHttpClientFactory,
    client: Option<HttpClient>,
}

impl LazyStreamingClient {
    pub(crate) fn new(factory: StreamingHttpClientFactory) -> Self {
        Self {
            factory,
            client: None,
        }
    }

    pub(crate) fn get(&mut self) -> EngineResult<HttpClient> {
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        let client = (self.factory)()?;
        self.client = Some(client.clone());
        Ok(client)
    }
}

pub(crate) fn stream_channel() -> (mpsc::Sender<StreamMsg>, mpsc::Receiver<StreamMsg>) {
    mpsc::channel(STREAM_CHANNEL_CAPACITY)
}

/// One accepted traversal of a source, independent of source/load identity.
/// Never wrap: a retired traversal must never become current again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct StreamRevision(u64);

impl StreamRevision {
    pub const INITIAL: Self = Self(0);

    pub fn checked_next(self) -> EngineResult<Self> {
        self.0.checked_add(1).map(Self).ok_or_else(|| {
            EngineError::from_detail(ErrorCode::InputSaturated, "stream revision exhausted")
        })
    }
}

/// A source clock counts playable frames after codec/container trimming. The
/// output position retains its rational phase; rounding is only explicit at
/// the final frame boundary, never accumulated one chunk at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StreamTimeline {
    frames: u64,
    input_rate: u32,
    output_rate: u32,
    channels: u32,
    output_frames: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OutputPosition {
    pub frame: u64,
    pub phase: u32,
    pub divisor: u32,
}

impl StreamTimeline {
    pub(crate) fn new(
        frames: u64,
        input_rate: u32,
        output_rate: u32,
        channels: u32,
    ) -> EngineResult<Self> {
        let valid_rate = |rate| {
            (crate::limits::MIN_SAMPLE_RATE..=crate::limits::MAX_SAMPLE_RATE).contains(&rate)
        };
        if frames == 0
            || !valid_rate(input_rate)
            || !valid_rate(output_rate)
            || !(1..=crate::limits::MAX_AUDIO_CHANNELS).contains(&channels)
        {
            return Err(EngineError::from_detail(
                ErrorCode::IoError,
                "invalid source timeline format",
            ));
        }
        let output_frames = u64::try_from(
            (u128::from(frames) * u128::from(output_rate)).div_ceil(u128::from(input_rate)),
        )
        .map_err(|_| EngineError::from_detail(ErrorCode::IoError, "source timeline overflow"))?;
        Ok(Self {
            frames,
            input_rate,
            output_rate,
            channels,
            output_frames,
        })
    }
    pub(crate) fn output_frames(self) -> u64 {
        self.output_frames
    }
    pub(crate) fn input_frames(self) -> u64 {
        self.frames
    }
    pub(crate) fn seconds(self) -> f64 {
        self.frames as f64 / self.input_rate as f64
    }
    pub(crate) fn format(self) -> (u32, u32) {
        (self.output_rate, self.channels)
    }
    pub(crate) fn seek_frame(self, seconds: f64) -> EngineResult<i64> {
        if !seconds.is_finite() {
            return Err(EngineError::from_detail(
                ErrorCode::InvalidArgument,
                "nonfinite seek time",
            ));
        }
        let frame = if seconds <= 0.0 {
            0
        } else if seconds >= self.seconds() {
            self.frames
        } else {
            (seconds * self.input_rate as f64) as u64
        };
        i64::try_from(frame).map_err(|_| {
            EngineError::from_detail(ErrorCode::InvalidArgument, "seek time exceeds source clock")
        })
    }
    pub(crate) fn position(self, frame: u64) -> EngineResult<OutputPosition> {
        if frame > self.frames {
            return Err(EngineError::from_detail(
                ErrorCode::InvalidArgument,
                "position beyond source timeline",
            ));
        }
        let scaled = u128::from(frame) * u128::from(self.output_rate);
        Ok(OutputPosition {
            frame: (scaled / u128::from(self.input_rate)) as u64,
            phase: (scaled % u128::from(self.input_rate)) as u32,
            divisor: self.input_rate,
        })
    }
}

/// One track's media clock is independent of the traversal's PCM extent.
/// Repeated output uses one global sampling grid, never a rounded per-loop size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StreamPresentation {
    timeline: StreamTimeline,
    cyclic: bool,
    output_end: Option<u64>,
}
impl StreamPresentation {
    pub(crate) fn single(timeline: StreamTimeline) -> Self {
        Self {
            timeline,
            cyclic: false,
            output_end: Some(timeline.output_frames()),
        }
    }
    pub(crate) fn repeated(timeline: StreamTimeline, cycles: Option<u64>) -> EngineResult<Self> {
        let output_end = cycles
            .map(|cycles| {
                if cycles == 0 {
                    return Err(EngineError::from_detail(
                        ErrorCode::InvalidArgument,
                        "zero presentation cycles",
                    ));
                }
                let frames = timeline.frames.checked_mul(cycles).ok_or_else(|| {
                    EngineError::from_detail(ErrorCode::IoError, "cyclic input clock overflow")
                })?;
                StreamTimeline::new(
                    frames,
                    timeline.input_rate,
                    timeline.output_rate,
                    timeline.channels,
                )
                .map(|t| t.output_frames())
            })
            .transpose()?;
        Ok(Self {
            timeline,
            cyclic: true,
            output_end,
        })
    }
    pub(crate) fn timeline(self) -> StreamTimeline {
        self.timeline
    }
    pub(crate) fn output_end(self) -> Option<u64> {
        self.output_end
    }
    pub(crate) fn is_cyclic(self) -> bool {
        self.cyclic
    }
    pub(crate) fn seconds_at(self, frame: u64) -> f64 {
        if !self.cyclic || self.output_end.is_some_and(|end| frame >= end) {
            return (frame as f64 / self.timeline.output_rate as f64).min(self.timeline.seconds());
        }
        let numerator = u128::from(frame) * u128::from(self.timeline.input_rate);
        let period = u128::from(self.timeline.frames) * u128::from(self.timeline.output_rate);
        (numerator % period) as f64
            / (f64::from(self.timeline.input_rate) * f64::from(self.timeline.output_rate))
    }
}

/// Rebuild a cyclic suffix on the global grid at the consumer's next output
/// frame. Decoder lookahead must not select which cycle disabling will finish.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StreamCutover {
    presentation: StreamPresentation,
    output_start: u64,
    input_origin: u64,
    cycles: Option<u64>,
}
impl StreamCutover {
    pub(crate) fn new(
        timeline: StreamTimeline,
        output_start: u64,
        enabled: bool,
        history: u64,
    ) -> EngineResult<Self> {
        let input = u64::from(timeline.input_rate);
        let output = u64::from(timeline.output_rate);
        let target =
            u64::try_from(u128::from(output_start) * u128::from(input) / u128::from(output))
                .map_err(|_| {
                    EngineError::from_detail(ErrorCode::InvalidArgument, "cutover input overflow")
                })?;
        let cycles = if enabled {
            None
        } else {
            Some((target / timeline.frames).checked_add(1).ok_or_else(|| {
                EngineError::from_detail(ErrorCode::InvalidArgument, "cutover cycle overflow")
            })?)
        };
        let presentation = StreamPresentation::repeated(timeline, cycles)?;
        let (mut a, mut b) = (input, output);
        while b != 0 {
            (a, b) = (b, a % b);
        }
        let period = input / a;
        let input_origin = if input == output {
            target
        } else {
            target.saturating_sub(history) / period * period
        };
        Ok(Self {
            presentation,
            output_start,
            input_origin,
            cycles,
        })
    }
    pub(crate) fn presentation(self) -> StreamPresentation {
        self.presentation
    }
    pub(crate) fn output_start(self) -> u64 {
        self.output_start
    }
    pub(crate) fn input_origin(self) -> u64 {
        self.input_origin
    }
    pub(crate) fn source_origin(self) -> (u64, u64) {
        let frames = self.presentation.timeline.frames;
        (self.input_origin % frames, self.input_origin / frames)
    }
    pub(crate) fn input_frames(self) -> u64 {
        self.presentation.timeline.frames
    }
    pub(crate) fn cycles(self) -> Option<u64> {
        self.cycles
    }
}

#[cfg(test)]
#[path = "stream_cutover_tests.rs"]
mod cutover_tests;

/// Accepted per-revision metadata; the source's single-track clock stays fixed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StreamTraversal {
    presentation: StreamPresentation,
    output_start: u64,
}
impl StreamTraversal {
    /// A media seek starts a canonical traversal on the track's output grid.
    /// Disabling loop here ends at the track endpoint, including an empty
    /// seek-to-duration suffix; it must not select the next audible cycle.
    pub(crate) fn seek(clock: StreamTimeline, target: u64, enabled: bool) -> EngineResult<Self> {
        let position = clock.position(target)?;
        Ok(Self {
            presentation: if enabled {
                StreamPresentation::repeated(clock, None)?
            } else {
                StreamPresentation::single(clock)
            },
            output_start: position.frame + u64::from(position.phase != 0),
        })
    }
    pub(crate) fn cutover(
        clock: StreamTimeline,
        output_start: u64,
        enabled: bool,
    ) -> EngineResult<Self> {
        let plan = StreamCutover::new(clock, output_start, enabled, 0)?;
        Ok(Self {
            presentation: plan.presentation(),
            output_start,
        })
    }
    pub(crate) fn presentation(self) -> StreamPresentation {
        self.presentation
    }
    pub(crate) fn output_start(self) -> u64 {
        self.output_start
    }
}

/// Every message, including errors and EOF, belongs to exactly one traversal.
pub enum StreamMsg {
    /// Audio format detected, can start playback
    Ready {
        revision: StreamRevision,
        sample_rate: u32,
        channels: u32,
    },
    /// New decoded samples available
    Samples {
        revision: StreamRevision,
        pcm: PcmChunk,
    },
    /// Download/decode complete
    Done { revision: StreamRevision },
    /// Error occurred
    Error {
        revision: StreamRevision,
        message: String,
    },
}

impl StreamMsg {
    pub fn revision(&self) -> StreamRevision {
        match self {
            Self::Ready { revision, .. }
            | Self::Samples { revision, .. }
            | Self::Done { revision }
            | Self::Error { revision, .. } => *revision,
        }
    }
}

/// Samples a recycled PCM buffer starts out able to hold.
///
/// Roughly a third of a second of 48 kHz stereo, which covers the network chunk
/// sizes a connection actually delivers. A larger chunk grows the buffer once and the
/// recycled buffer keeps the larger capacity, so growth is paid at most once per
/// stream rather than once per chunk.
const PCM_CHUNK_SAMPLES: usize = 32 * 1024;

/// Decoded PCM on loan from the stream that produced it.
///
/// A decoded chunk has to change threads, so it has to be owned; without a way
/// back, every chunk is one allocation on the streaming worker and one free on
/// the audio thread, forever, for as long as anything is playing. Handing the
/// buffer back is a `Drop` rather than a call the consumer has to remember,
/// which is the same instrument the render path's command vectors use: there is
/// no recycle step to forget.
pub struct PcmChunk {
    buffer: pcm_buffer::Buffer,
    home: mpsc::Sender<pcm_buffer::Buffer>,
}

impl PcmChunk {
    /// The buffer to decode into. Only the producer has one of these before it
    /// is sent, so this cannot be used to mutate a chunk in flight.
    pub(crate) fn buffer_mut(&mut self) -> &mut Vec<f32> {
        &mut self.buffer
    }
}

impl std::ops::Deref for PcmChunk {
    type Target = [f32];

    fn deref(&self) -> &Self::Target {
        &self.buffer
    }
}

impl Drop for PcmChunk {
    fn drop(&mut self) {
        let mut buffer = std::mem::take(&mut self.buffer);
        buffer.clear();
        // Never blocks and never grows. A full return channel means the producer
        // already holds more buffers than it can be using, and a closed one means
        // the stream is gone. Managed buffers then transfer their same allocation
        // to CPU disposal; legacy standalone pools keep synchronous ownership.
        let _ = self.home.try_send(buffer);
    }
}

/// The producer's half of the loan: hands out chunks, takes back what returns.
pub(crate) struct PcmPool {
    home: mpsc::Sender<pcm_buffer::Buffer>,
    returns: mpsc::Receiver<pcm_buffer::Buffer>,
    cpu: Option<migo_io::audio_cpu::AudioCpu>,
    slots: Option<Arc<tokio::sync::Semaphore>>,
}

impl PcmPool {
    pub(crate) fn new() -> Self {
        // One slot more than can be in flight, so a healthy stream never fails a
        // return for want of room.
        let (home, returns) = mpsc::channel(STREAM_CHANNEL_CAPACITY + 1);
        Self {
            home,
            returns,
            cpu: None,
            slots: None,
        }
    }

    /// Production pools retain CPU-only disposal when returns are full/closed.
    pub(crate) fn with_cpu(cpu: migo_io::audio_cpu::AudioCpu) -> Self {
        Self {
            cpu: Some(cpu),
            // Original return cache (C+1), publication channel C, one consumer,
            // old pending output and one applying control/working CPU loan.
            slots: Some(Arc::new(tokio::sync::Semaphore::new(
                2 * STREAM_CHANNEL_CAPACITY + 4,
            ))),
            ..Self::new()
        }
    }

    fn chunk(&self, buffer: pcm_buffer::Buffer) -> PcmChunk {
        PcmChunk {
            buffer,
            home: self.home.clone(),
        }
    }
    fn allocate(&self, slot: Option<tokio::sync::OwnedSemaphorePermit>) -> PcmChunk {
        self.chunk(pcm_buffer::Buffer::new(
            Vec::with_capacity(PCM_CHUNK_SAMPLES),
            self.cpu.as_ref(),
            slot,
        ))
    }
    // Standalone callers retain their old synchronous API. Production uses
    // acquire so retired physical loans cannot be replaced without admission.
    #[cfg(test)]
    pub(crate) fn take(&mut self) -> PcmChunk {
        if let Ok(buffer) = self.returns.try_recv() {
            return self.chunk(buffer);
        }
        let slot = self.slots.as_ref().map(|slots| {
            slots
                .clone()
                .try_acquire_owned()
                .expect("managed PCM loans require async admission")
        });
        self.allocate(slot)
    }
    pub(crate) async fn acquire(&mut self) -> PcmChunk {
        if let Ok(buffer) = self.returns.try_recv() {
            return self.chunk(buffer);
        }
        let Some(slots) = self.slots.as_ref() else {
            return self.allocate(None);
        };
        tokio::select! {
            biased;
            Some(buffer)=self.returns.recv()=>self.chunk(buffer),
            slot=slots.clone().acquire_owned()=>self.allocate(Some(slot.expect("PCM physical loan admission closed"))),
        }
    }
}

/// Shared state for streaming progress
pub struct StreamingState {
    /// Total bytes downloaded
    pub bytes_downloaded: AtomicU64,
    /// Total bytes expected (0 if unknown)
    pub bytes_total: AtomicU64,
    /// Whether download is complete
    pub download_complete: AtomicBool,
    /// Whether cancelled
    pub cancelled: AtomicBool,
    /// Monotonic real output consumption, independent of seeks and revisions.
    rendered_frames: AtomicU64,
    /// Event-driven cancellation edge for tasks blocked on network I/O.
    cancel_notify: Notify,
    /// A reusable producer keeps its source and command receiver alive at EOF.
    retain_after_eof: bool,
    /// URL capability is selected asynchronously; a real port activates retention.
    require_control_port: bool,
    /// Bound once by the CPU decoder to this exact source, across revisions.
    timeline: std::sync::OnceLock<StreamTimeline>,
    presentation: std::sync::OnceLock<StreamPresentation>,
    controls: std::sync::OnceLock<mpsc::Sender<crate::stream_control::Command>>,
    /// Test observation of an actual queued publication wait, after CPU return.
    #[cfg(test)]
    pub(crate) publication_waiting: AtomicBool,
}

impl StreamingState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(crate) fn new_reusable() -> Arc<Self> {
        Arc::new(Self {
            retain_after_eof: true,
            ..Self::default()
        })
    }

    pub(crate) fn new_url() -> Arc<Self> {
        Self::new_optional_controls()
    }

    pub(crate) fn new_optional_controls() -> Arc<Self> {
        Arc::new(Self {
            retain_after_eof: true,
            require_control_port: true,
            ..Self::default()
        })
    }

    pub(crate) fn accepts_controls(&self) -> bool {
        self.retain_after_eof
    }

    pub(crate) fn retains_source(&self) -> bool {
        self.retain_after_eof && (!self.require_control_port || self.controls.get().is_some())
    }

    pub(crate) fn timeline(&self) -> Option<StreamTimeline> {
        self.timeline.get().copied()
    }

    pub(crate) fn presentation(&self) -> Option<StreamPresentation> {
        self.presentation.get().copied()
    }

    pub(crate) fn bind_presentation(&self, presentation: StreamPresentation) -> EngineResult<()> {
        self.bind_timeline(presentation.timeline())?;
        if *self.presentation.get_or_init(|| presentation) != presentation {
            return Err(EngineError::from_detail(
                ErrorCode::IoError,
                "changed source presentation",
            ));
        }
        Ok(())
    }

    pub(crate) fn control_sender(&self) -> Option<&mpsc::Sender<crate::stream_control::Command>> {
        self.controls.get()
    }

    pub(crate) fn install_controls(
        &self,
        sender: mpsc::Sender<crate::stream_control::Command>,
    ) -> EngineResult<()> {
        if !self.accepts_controls() || self.is_cancelled() {
            return Err(EngineError::from_detail(
                ErrorCode::InvalidArgument,
                "inactive source control port",
            ));
        }
        self.controls.set(sender).map_err(|_| {
            EngineError::from_detail(
                ErrorCode::InvalidArgument,
                "source control port already installed",
            )
        })
    }

    pub(crate) fn bind_timeline(&self, timeline: StreamTimeline) -> EngineResult<()> {
        if self.is_cancelled() || *self.timeline.get_or_init(|| timeline) != timeline {
            return Err(EngineError::from_detail(
                ErrorCode::IoError,
                "retired or changed source timeline",
            ));
        }
        Ok(())
    }

    pub fn cancel(&self) {
        if !self.cancelled.swap(true, Ordering::AcqRel) {
            self.cancel_notify.notify_waiters();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub(crate) fn rendered_frames(&self) -> u64 {
        self.rendered_frames.load(Ordering::Relaxed)
    }

    pub(crate) fn record_rendered_frames(&self, frames: usize) {
        if frames != 0 && !self.is_cancelled() {
            // Saturation stops future credit rather than wrapping into a new
            // epoch. Only the audio consumer calls this, once per mixed block.
            let _ =
                self.rendered_frames
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                        Some(old.saturating_add(frames as u64))
                    });
        }
    }

    /// Wait without polling. Registering before the second level check closes
    /// the check-to-sleep race with `notify_waiters`, which does not retain a
    /// permit for future waiters.
    pub(crate) async fn cancelled(&self) {
        loop {
            let notified = self.cancel_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

impl Default for StreamingState {
    fn default() -> Self {
        Self {
            bytes_downloaded: AtomicU64::new(0),
            bytes_total: AtomicU64::new(0),
            download_complete: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            rendered_frames: AtomicU64::new(0),
            cancel_notify: Notify::new(),
            retain_after_eof: false,
            require_control_port: false,
            timeline: std::sync::OnceLock::new(),
            presentation: std::sync::OnceLock::new(),
            controls: std::sync::OnceLock::new(),
            #[cfg(test)]
            publication_waiting: AtomicBool::new(false),
        }
    }
}

#[cfg(test)]
mod rendered_progress_tests {
    use super::*;

    #[test]
    fn consumed_credit_saturates_and_retirement_cannot_mint_more() {
        let source = StreamingState::new();
        source
            .rendered_frames
            .store(u64::MAX - 3, Ordering::Relaxed);
        source.record_rendered_frames(8);
        assert_eq!(source.rendered_frames(), u64::MAX);
        source.record_rendered_frames(8);
        assert_eq!(source.rendered_frames(), u64::MAX);
        let source = StreamingState::new();
        source.record_rendered_frames(3);
        source.cancel();
        source.record_rendered_frames(8);
        assert_eq!(source.rendered_frames(), 3);
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NetworkWait<T> {
    Ready(T),
    Cancelled,
    TimedOut,
}

/// Race one network await against the event-driven cancellation edge and an
/// inactivity deadline. Generic/monomorphized so the production path allocates
/// no boxed future or polling task.
pub(crate) async fn wait_for_network<T, F>(
    state: &StreamingState,
    timeout: Duration,
    operation: F,
) -> NetworkWait<T>
where
    F: Future<Output = T>,
{
    tokio::select! {
        biased;
        _ = state.cancelled() => NetworkWait::Cancelled,
        result = tokio::time::timeout(timeout, operation) => match result {
            Ok(value) => NetworkWait::Ready(value),
            Err(_) => NetworkWait::TimedOut,
        },
    }
}

/// Shared single-worker runtime for all audio streaming downloads.
static STREAM_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

pub(crate) fn get_stream_runtime() -> &'static tokio::runtime::Runtime {
    STREAM_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("audio-stream")
            .enable_all()
            .build()
            .expect("Failed to create audio streaming runtime")
    })
}

/// Start streaming download and decode
/// Returns a receiver for decoded audio chunks
pub fn start_streaming_download(
    cpu: migo_io::audio_cpu::AudioCpu,
    client: HttpClient,
    url: String,
    state: Arc<StreamingState>,
    target_sample_rate: u32,
) -> mpsc::Receiver<StreamMsg> {
    let (tx, rx) = stream_channel();

    get_stream_runtime().spawn(crate::streaming_transport::run_with_source::<
        crate::streaming_codecs::AutoDecoder,
        crate::streaming_http::IndexedHttp,
    >(cpu, client, url, tx, state, move |_| {
        crate::streaming_codecs::AutoDecoder::http(target_sample_rate)
    }));

    rx
}

/// Product URL loads carry the session's existing private Fs spool authority.
pub fn start_streaming_download_with_spool(
    cpu: migo_io::audio_cpu::AudioCpu,
    client: HttpClient,
    url: String,
    state: Arc<StreamingState>,
    target_sample_rate: u32,
    spool: Option<Arc<dyn shared::audio_file::AudioHttpSpool>>,
) -> mpsc::Receiver<StreamMsg> {
    let Some(spool) = spool else {
        return start_streaming_download(cpu, client, url, state, target_sample_rate);
    };
    let (tx, rx) = stream_channel();
    get_stream_runtime().spawn(crate::streaming_http_prepare::run(
        cpu,
        client,
        url,
        tx,
        state,
        target_sample_rate,
        spool,
    ));
    rx
}

/// Producer-owned protocol ledger. Seeking reuses the source's PCM format while
/// starting a fresh Ready/Samples/Done sequence. No atomics on the render path.
#[derive(Default)]
pub(crate) struct StreamPublication {
    revision: StreamRevision,
    format: Option<(u32, u32)>,
    ready_sent: bool,
    total_samples: u64,
    expected_samples: Option<u64>,
    presentation: Option<StreamPresentation>,
    finished: bool,
}

impl StreamPublication {
    pub(crate) fn revision(&self) -> StreamRevision {
        self.revision
    }

    /// Call only after a source reposition has succeeded. Rejected requests
    /// leave the old traversal and its buffered messages valid.
    pub(crate) fn begin(
        &mut self,
        revision: StreamRevision,
        state: &StreamingState,
    ) -> EngineResult<()> {
        if revision <= self.revision {
            return Err(EngineError::from_detail(
                ErrorCode::InvalidArgument,
                "stream revision must advance",
            ));
        }
        self.revision = revision;
        self.ready_sent = false;
        self.total_samples = 0;
        self.expected_samples = None;
        self.presentation = None;
        self.finished = false;
        state.download_complete.store(false, Ordering::Release);
        Ok(())
    }

    pub(crate) async fn finish(
        &mut self,
        tx: &mpsc::Sender<StreamMsg>,
        state: &StreamingState,
        eof: bool,
        remaining: usize,
    ) -> EngineResult<bool> {
        self.validate_finish(eof, remaining)?;
        let permit = tokio::select! {
            biased;
            _ = state.cancelled() => return Ok(false),
            result = tx.reserve() => match result { Ok(permit) => permit, Err(_) => return Ok(false) },
        };
        self.finish_with(permit, state);
        Ok(true)
    }
}

/// One validated PCM frame waiting for channel slots. The producer keeps the
/// loan here until a permit is ready, so a soft control cannot lose its bytes.
pub(crate) struct PendingPublication {
    pcm: Option<PcmChunk>,
    sample_rate: u32,
    channels: u32,
    total: u64,
}

impl StreamPublication {
    pub(crate) fn has_timeline(&self) -> bool {
        self.presentation.is_some() || self.expected_samples.is_some()
    }

    pub(crate) fn bind_presentation(
        &mut self,
        presentation: StreamPresentation,
        output_start: u64,
    ) -> EngineResult<()> {
        if self.presentation.is_some_and(|old| old != presentation) || self.finished {
            return Err(EngineError::from_detail(
                ErrorCode::IoError,
                "publication presentation changed",
            ));
        }
        if !presentation.is_cyclic() {
            self.bind_timeline(presentation.timeline(), output_start)?;
        } else {
            let expected = presentation
                .output_end()
                .map(|end| {
                    end.checked_sub(output_start)
                        .and_then(|n| n.checked_mul(u64::from(presentation.timeline().format().1)))
                        .ok_or_else(|| {
                            EngineError::from_detail(
                                ErrorCode::IoError,
                                "invalid cyclic publication extent",
                            )
                        })
                })
                .transpose()?;
            if self.presentation.is_some_and(|old| old != presentation)
                || self
                    .expected_samples
                    .is_some_and(|old| Some(old) != expected)
                || self
                    .format
                    .is_some_and(|old| old != presentation.timeline().format())
                || expected.is_some_and(|end| self.total_samples > end)
                || self.finished
            {
                return Err(EngineError::from_detail(
                    ErrorCode::IoError,
                    "cyclic publication extent changed",
                ));
            }
            self.expected_samples = expected;
            self.format = Some(presentation.timeline().format());
        }
        if self.presentation.is_some_and(|old| old != presentation) {
            return Err(EngineError::from_detail(
                ErrorCode::IoError,
                "publication presentation changed",
            ));
        }
        self.presentation = Some(presentation);
        Ok(())
    }

    pub(crate) fn bind_timeline(
        &mut self,
        timeline: StreamTimeline,
        output_start: u64,
    ) -> EngineResult<()> {
        if self
            .presentation
            .is_some_and(|p| p != StreamPresentation::single(timeline))
        {
            return Err(EngineError::from_detail(
                ErrorCode::IoError,
                "publication clock cannot change traversal mode",
            ));
        }
        let expected = timeline
            .output_frames()
            .checked_sub(output_start)
            .and_then(|frames| frames.checked_mul(u64::from(timeline.format().1)))
            .ok_or_else(|| {
                EngineError::from_detail(ErrorCode::IoError, "invalid publication timeline")
            })?;
        if self.expected_samples.is_some_and(|old| old != expected)
            || self.format.is_some_and(|old| old != timeline.format())
            || self.total_samples > expected
            || self.finished
        {
            return Err(EngineError::from_detail(
                ErrorCode::IoError,
                "publication timeline changed",
            ));
        }
        self.expected_samples = Some(expected);
        self.format = Some(timeline.format());
        Ok(())
    }

    pub(crate) fn prepare_chunk(
        &mut self,
        pcm: PcmChunk,
        sample_rate: u32,
        channels: u32,
    ) -> EngineResult<PendingPublication> {
        if self.finished {
            return Err(EngineError::from_detail(
                ErrorCode::IoError,
                "PCM after traversal EOF",
            ));
        }
        let has_format = sample_rate > 0 && channels > 0;
        if (sample_rate != 0 || channels != 0) && !has_format
            || !pcm.is_empty() && (!has_format || !pcm.len().is_multiple_of(channels as usize))
        {
            return Err(EngineError::from_detail(
                ErrorCode::Internal,
                "decoded audio samples had no complete PCM format",
            ));
        }
        if has_format {
            let format = (sample_rate, channels);
            if self.format.is_some_and(|old| old != format) {
                return Err(EngineError::from_detail(
                    ErrorCode::IoError,
                    "stream PCM format changed",
                ));
            }
            self.format = Some(format);
        }
        let total = self
            .total_samples
            .checked_add(pcm.len() as u64)
            .ok_or_else(|| {
                EngineError::from_detail(ErrorCode::Internal, "stream PCM counter overflow")
            })?;
        if self
            .expected_samples
            .is_some_and(|expected| total > expected)
        {
            return Err(EngineError::from_detail(
                ErrorCode::IoError,
                "PCM exceeds publication timeline",
            ));
        }
        Ok(PendingPublication {
            pcm: Some(pcm),
            sample_rate,
            channels,
            total,
        })
    }

    pub(crate) fn has_pending(&self, pending: &PendingPublication) -> bool {
        !self.ready_sent && pending.sample_rate > 0
            || pending.pcm.as_ref().is_some_and(|pcm| !pcm.is_empty())
    }

    pub(crate) fn publish_next(
        &mut self,
        pending: &mut PendingPublication,
        permit: mpsc::Permit<'_, StreamMsg>,
    ) {
        if !self.ready_sent && pending.sample_rate > 0 {
            permit.send(StreamMsg::Ready {
                revision: self.revision,
                sample_rate: pending.sample_rate,
                channels: pending.channels,
            });
            self.ready_sent = true;
        } else {
            let pcm = pending.pcm.take().expect("pending PCM owner");
            assert!(!pcm.is_empty(), "publishing requires pending PCM");
            permit.send(StreamMsg::Samples {
                revision: self.revision,
                pcm,
            });
            self.total_samples = pending.total;
        }
    }

    pub(crate) fn validate_finish(&self, eof: bool, remaining: usize) -> EngineResult<()> {
        let complete = self
            .expected_samples
            .map_or(self.total_samples > 0, |n| n == self.total_samples);
        if self.finished
            || !eof
            || remaining != 0
            || !self.ready_sent
            || !complete
            || self.presentation.is_some_and(|p| p.output_end().is_none())
        {
            return Err(EngineError::from_detail(
                ErrorCode::IoError,
                "stream decoder finished without complete audio input",
            ));
        }
        Ok(())
    }

    pub(crate) fn finish_with(
        &mut self,
        permit: mpsc::Permit<'_, StreamMsg>,
        state: &StreamingState,
    ) {
        permit.send(StreamMsg::Done {
            revision: self.revision,
        });
        self.finished = true;
        state.download_complete.store(true, Ordering::Release);
    }
}

/// One format invariant for ordinary decoding, EOF flush and resumed traversal.
pub(crate) async fn publish_stream_chunk(
    tx: &mpsc::Sender<StreamMsg>,
    state: &StreamingState,
    pcm: PcmChunk,
    sample_rate: u32,
    channels: u32,
    publication: &mut StreamPublication,
) -> EngineResult<bool> {
    let mut pending = publication.prepare_chunk(pcm, sample_rate, channels)?;
    while publication.has_pending(&pending) {
        let permit = tokio::select! {
            biased;
            _ = state.cancelled() => return Ok(false),
            result = tx.reserve() => match result { Ok(permit) => permit, Err(_) => return Ok(false) },
        };
        publication.publish_next(&mut pending, permit);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mp3_fixture;
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    use migo_executor_probe::{PATIENCE, SharedExecutor, assert_leaves_the_executor_free};
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use tokio::sync::mpsc::error::TrySendError;

    /// Not MP3, and deliberately so: the decoder keeps what it cannot yet decode, so
    /// the buffered length is a value only the real decoder produces.
    const A_CHUNK: &[u8] = b"bytes no frame ends in";

    #[test]
    fn the_decode_step_leaves_the_shared_streaming_worker_free() {
        // `STREAM_RUNTIME` is process-wide and single-worker, so a decode that ran on
        // it would stall every other session's download for as long as the decode
        // took -- which is as long as the chunk is, not as long as anyone budgeted.
        let owner = migo_io::scheduler::IoScheduler::new(0);
        let buffered = assert_leaves_the_executor_free(
            SharedExecutor {
                step: "streaming MP3 decode",
                executor: "the process-wide audio streaming worker",
                patience: PATIENCE,
            },
            get_stream_runtime(),
            |cpu| async move {
                let (_decoder, buffered) =
                    OffWorker::new(Mp3StreamDecoder::new(48_000), owner.audio_cpu())
                        .with(move |decoder| {
                            cpu.occupy();
                            decoder.push_data(A_CHUNK);
                            decoder.decode_into(&mut Vec::new());
                            decoder.buffered_len()
                        })
                        .await
                        .expect("the decode step must complete");
                buffered
            },
        );

        assert_eq!(
            buffered,
            A_CHUNK.len(),
            "the step must have run against the real decoder, not a stand-in"
        );
    }

    /// The steady-state allocation gate, applied to the streaming
    /// refill: one network chunk in, its decoded PCM out.
    ///
    /// The unit is a chunk because that is what repeats -- a track is thousands
    /// of them -- and everything inside it is per-frame, so an allocation here is
    /// multiplied by however many frames the chunk carried. Resampling is on
    /// (the fixture is 44.1 kHz, the device 48 kHz), which is the configuration
    /// most Android devices actually run.
    ///
    /// The warm-up covers the decoder's first frame, the resampler's
    /// construction, every buffer's first growth to a chunk's worth, and the
    /// return channel's block list reaching its working set -- tokio hands out
    /// message slots 32 to a block and recycles them, so the list grows once and
    /// then never again. Forty iterations crosses that boundary; the property was
    /// checked over 256 measured iterations before this window was chosen, so the
    /// number is a warm-up and not a hiding place.
    #[test]
    fn a_steady_state_streaming_chunk_never_reaches_the_heap() {
        const WARMUP: usize = 40;
        const MEASURED: usize = 128;
        const FRAMES_PER_CHUNK: usize = 2;

        let chunk_bytes = FRAMES_PER_CHUNK * mp3_fixture::FRAME_BYTES;
        // Built before the measured window, and long enough that no iteration
        // runs out of input and quietly measures a decoder with nothing to do.
        let source = mp3_fixture::stream(FRAMES_PER_CHUNK * (WARMUP + MEASURED) + 4);
        let chunks: Vec<&[u8]> = source.chunks_exact(chunk_bytes).collect();
        assert!(chunks.len() >= WARMUP + MEASURED);

        let mut decoder = Mp3StreamDecoder::new(48_000);
        let mut pool = PcmPool::new();
        let mut decoded_total = 0usize;

        assert_no_steady_state_allocation(
            Burst {
                path: "audio: one streaming chunk decoded and resampled",
                warmup: WARMUP,
                measured: MEASURED,
            },
            |iteration| {
                decoder.push_data(chunks[iteration]);
                // Taken from the pool and dropped again at the end of the
                // iteration, which is the whole loan: a burst that kept its
                // buffers would measure the pool draining, not the steady state.
                let mut pcm = pool.take();
                decoder.decode_into(pcm.buffer_mut());
                decoded_total += pcm.len();
            },
        );

        assert!(
            decoded_total > 0,
            "the burst must have decoded audio, or it proves nothing"
        );
    }

    /// A stream cut into chunks must decode to exactly what one pass over the
    /// same bytes produces.
    ///
    /// This is the correctness half of the same change, and it is a reftest
    /// rather than a golden file because both sides are computed in the same run:
    /// no baseline to go stale, and no claim about any particular recording. The
    /// chunk size is deliberately not a multiple of the frame size, so frames are
    /// split at every offset within them.
    ///
    /// What it pins is decoder *state*. Rebuilding the decoder per chunk -- which
    /// is what constructing one inside the decode step amounted to -- discards
    /// the bit reservoir, and a frame whose main data lives there then decodes to
    /// nothing at all: silently short audio, no error anywhere.
    #[test]
    fn a_chunked_stream_decodes_to_what_one_pass_over_the_same_bytes_does() {
        const FRAMES: usize = 24;
        // Coprime with the frame size, so no chunk boundary lands on a frame one.
        const CHUNK_BYTES: usize = 137;

        let source = mp3_fixture::stream(FRAMES);
        let one_pass = crate::decoder::mp3::decode(&source).expect("the fixture must decode");

        // Same rate in and out, so the two sides are comparable sample for sample.
        let mut decoder = Mp3StreamDecoder::new(mp3_fixture::SAMPLE_RATE);
        let mut chunked = Vec::new();
        for chunk in source.chunks(CHUNK_BYTES) {
            decoder.push_data(chunk);
            decoder.decode_into(&mut chunked);
        }
        decoder.flush_into(&mut chunked);

        assert_eq!(
            one_pass.samples.len(),
            FRAMES * mp3_fixture::SAMPLES_PER_FRAME,
            "the one-pass side must itself be complete, or this compares two \
             equally broken decodes"
        );
        assert_eq!(
            chunked.len(),
            one_pass.samples.len(),
            "a chunked stream lost or invented samples"
        );
        assert_eq!(chunked, one_pass.samples);
    }

    /// A stream that ends in bytes that are not audio must still decode.
    ///
    /// An ID3v1 tag is 128 bytes at the very end of a large fraction of real MP3
    /// files, and minimp3 will not accept a frame it cannot chain to a successor,
    /// so the frame immediately before such a tag is lost. That single frame —
    /// 26 ms at the end of a track — is the accepted cost; what is asserted here
    /// is that it is *one* frame and not the stream.
    ///
    /// Bytes that are not audio, before or after the audio, must cost no frames.
    ///
    /// An ID3v1 tag is 128 bytes at the end of a large fraction of real MP3
    /// files, and an ID3v2 tag at the front runs to kilobytes. Both defeat
    /// minimp3's requirement that a frame be chainable to a successor, and the
    /// lengths matter independently: a stream shorter than the lookahead decodes
    /// nothing until the flush and so meets a decoder that has never run, while a
    /// longer one arrives there warm.
    ///
    /// Every combination is asserted **exact**. Losing "just the last frame" was
    /// the behaviour before the flush learned to isolate frames, and it is not a
    /// tolerance worth keeping: a rule that permits one lost frame permits the
    /// mechanism to stop working and lose it every time.
    #[test]
    fn bytes_that_are_not_audio_cost_no_frames() {
        let id3v1 = {
            let mut tag = b"TAG".to_vec();
            tag.resize(128, 0);
            tag
        };
        // Larger than any frame, so the flush cannot isolate its way past it and
        // the in-place fallback is what has to carry this case.
        let id3v2 = vec![0u8; 4096];

        for frames in [2usize, 6, 40] {
            for (label, leading, trailing) in [
                ("a trailing tag", [].as_slice(), id3v1.as_slice()),
                ("a leading tag", id3v2.as_slice(), [].as_slice()),
                ("tags at both ends", id3v2.as_slice(), id3v1.as_slice()),
            ] {
                let mut source = leading.to_vec();
                source.extend_from_slice(&mp3_fixture::stream(frames));
                source.extend_from_slice(trailing);

                let mut decoder = Mp3StreamDecoder::new(mp3_fixture::SAMPLE_RATE);
                let mut out = Vec::new();
                for chunk in source.chunks(137) {
                    decoder.push_data(chunk);
                    decoder.decode_into(&mut out);
                }
                decoder.flush_into(&mut out);

                assert_eq!(
                    out.len() / mp3_fixture::SAMPLES_PER_FRAME,
                    frames,
                    "a {frames}-frame stream with {label} must decode every frame"
                );
            }
        }
    }

    /// A tag whose bytes look like a frame header, and a first frame that points into a reservoir
    /// nobody filled: neither may cost the frames after it, whether the stream is short enough to
    /// meet only the flush or long enough to decode in place.
    #[test]
    fn a_false_frame_costs_nothing_and_a_frame_without_its_reservoir_costs_only_itself() {
        let tag = |side_info: u8| {
            let mut body = vec![0u8; 16];
            body.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
            body.extend_from_slice(&[side_info; 32]);
            body.resize(600, 0);
            let n = body.len();
            let mut tag = b"ID3\x03\x00\x00".to_vec();
            tag.extend([
                (n >> 21) as u8 & 0x7F,
                (n >> 14) as u8 & 0x7F,
                (n >> 7) as u8 & 0x7F,
                n as u8 & 0x7F,
            ]);
            tag.extend_from_slice(&body);
            tag
        };

        for frames in [3usize, 6, 40] {
            let stream = mp3_fixture::stream(frames);
            for (label, source, expected) in [
                (
                    "behind a tag holding a false frame that decodes to nothing",
                    [tag(0xFF), stream.clone()].concat(),
                    frames,
                ),
                (
                    "behind a tag holding a false frame that decodes to silence",
                    [tag(0x00), stream.clone()].concat(),
                    frames,
                ),
                (
                    "whose first frame has no reservoir",
                    stream[mp3_fixture::FRAME_BYTES..].to_vec(),
                    frames - 2,
                ),
            ] {
                let mut decoder = Mp3StreamDecoder::new(mp3_fixture::SAMPLE_RATE);
                let mut out = Vec::new();
                for chunk in source.chunks(137) {
                    decoder.push_data(chunk);
                    decoder.decode_into(&mut out);
                }
                decoder.flush_into(&mut out);
                assert_eq!(
                    out.len() / mp3_fixture::SAMPLES_PER_FRAME,
                    expected,
                    "{frames} frames {label}"
                );
            }
        }
    }

    /// The loan has to come back, and come back empty.
    ///
    /// Identity is asserted by address: a pool that quietly allocated a fresh
    /// buffer each time would satisfy every other observable property here, and
    /// the allocation gate above is the only other thing that would notice.
    #[test]
    fn a_returned_chunk_is_the_buffer_the_next_one_is_written_into() {
        super::pcm_retirement_tests::retirement();
        let mut pool = PcmPool::new();

        let mut first = pool.take();
        first.buffer_mut().extend_from_slice(&[0.5f32; 128]);
        let lent = first.buffer.as_ptr();
        assert_eq!(first.len(), 128);
        drop(first);

        let second = pool.take();
        assert!(second.is_empty(), "a returned buffer must come back empty");
        assert_eq!(
            second.buffer.as_ptr(),
            lent,
            "the next chunk must be written into the buffer the last one returned"
        );
    }

    #[test]
    fn stream_channel_applies_backpressure_at_capacity() {
        let (tx, mut rx) = stream_channel();

        for _ in 0..STREAM_CHANNEL_CAPACITY {
            assert!(
                tx.try_send(StreamMsg::Done {
                    revision: crate::streaming::StreamRevision::INITIAL
                })
                .is_ok()
            );
        }
        assert!(matches!(
            tx.try_send(StreamMsg::Done {
                revision: crate::streaming::StreamRevision::INITIAL
            }),
            Err(TrySendError::Full(_))
        ));

        assert!(rx.try_recv().is_ok());
        assert!(
            tx.try_send(StreamMsg::Done {
                revision: crate::streaming::StreamRevision::INITIAL
            })
            .is_ok()
        );
    }

    fn counting_factory(calls: Arc<AtomicUsize>) -> StreamingHttpClientFactory {
        Arc::new(move || {
            calls.fetch_add(1, AtomicOrdering::Relaxed);
            migo_services::network::client::create_policy_http_client(
                "migo-test",
                false,
                &shared::network_policy::NetworkPolicy::default(),
                "audio",
                migo_io::pools::IoPools::new(0),
            )
        })
    }

    #[test]
    fn streaming_client_is_lazy_and_built_once_for_multiple_tracks() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut client = LazyStreamingClient::new(counting_factory(calls.clone()));

        assert_eq!(calls.load(AtomicOrdering::Relaxed), 0);
        let _first = client.get().expect("first client");
        let _second = client.get().expect("reused client");
        assert_eq!(calls.load(AtomicOrdering::Relaxed), 1);
    }

    #[test]
    fn failed_client_build_is_not_cached() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_factory = calls.clone();
        let factory: StreamingHttpClientFactory = Arc::new(move || {
            calls_for_factory.fetch_add(1, AtomicOrdering::Relaxed);
            Err(EngineError::from_detail(
                ErrorCode::IoError,
                "injected client build failure",
            ))
        });
        let mut client = LazyStreamingClient::new(factory);

        assert!(client.get().is_err());
        assert!(client.get().is_err());
        assert_eq!(calls.load(AtomicOrdering::Relaxed), 2);
    }

    #[test]
    fn streaming_clients_are_isolated_per_host_instance() {
        let host_a_calls = Arc::new(AtomicUsize::new(0));
        let host_b_calls = Arc::new(AtomicUsize::new(0));
        let mut host_a = LazyStreamingClient::new(counting_factory(host_a_calls.clone()));
        let mut host_b = LazyStreamingClient::new(counting_factory(host_b_calls.clone()));

        let _ = host_a.get().unwrap();
        let _ = host_a.get().unwrap();
        let _ = host_b.get().unwrap();
        assert_eq!(host_a_calls.load(AtomicOrdering::Relaxed), 1);
        assert_eq!(host_b_calls.load(AtomicOrdering::Relaxed), 1);
    }

    #[test]
    fn cancellation_waiter_handles_pre_cancel_and_in_flight_cancel() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let pre_cancelled = StreamingState::new();
            pre_cancelled.cancel();
            tokio::time::timeout(
                std::time::Duration::from_millis(50),
                pre_cancelled.cancelled(),
            )
            .await
            .expect("pre-cancelled waiter must resolve");

            let in_flight = StreamingState::new();
            let waiter = in_flight.cancelled();
            tokio::pin!(waiter);
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(10), &mut waiter)
                    .await
                    .is_err(),
                "waiter must remain pending before cancellation"
            );
            in_flight.cancel();
            tokio::time::timeout(std::time::Duration::from_millis(50), waiter)
                .await
                .expect("registered waiter must observe cancellation");
        });
    }

    #[test]
    fn network_wait_covers_ready_timeout_and_registered_cancellation() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let active = StreamingState::new();
            assert_eq!(
                wait_for_network(&active, Duration::from_millis(50), std::future::ready(7u8),)
                    .await,
                NetworkWait::Ready(7)
            );
            assert_eq!(
                wait_for_network(
                    &active,
                    Duration::from_millis(10),
                    std::future::pending::<()>(),
                )
                .await,
                NetworkWait::TimedOut
            );

            let cancelling = StreamingState::new();
            let wait = wait_for_network(
                &cancelling,
                Duration::from_secs(1),
                std::future::pending::<()>(),
            );
            tokio::pin!(wait);
            assert!(
                tokio::time::timeout(Duration::from_millis(10), &mut wait)
                    .await
                    .is_err(),
                "network wait must be registered and pending before cancel"
            );
            cancelling.cancel();
            assert_eq!(
                tokio::time::timeout(Duration::from_millis(50), wait)
                    .await
                    .expect("cancellation must wake network wait"),
                NetworkWait::Cancelled
            );
        });
    }

    #[test]
    fn flush_reports_output_rate_and_short_stream_format() {
        let mut decoder = Mp3StreamDecoder::new(48_000);
        let mut body = Vec::new();
        decoder.push_data(&mp3_fixture::stream(6));
        assert_eq!(decoder.decode_into(&mut body), (0, 0));

        let mut flushed = Vec::new();
        let (sample_rate, channels) = decoder.flush_into(&mut flushed);
        assert_eq!((sample_rate, channels), (48_000, 2));
        assert!(!flushed.is_empty(), "short stream must decode during flush");
    }

    #[test]
    fn body_reports_output_rate_when_resampling() {
        let mut decoder = Mp3StreamDecoder::new(48_000);
        decoder.push_data(&mp3_fixture::stream(40));
        let mut body = Vec::new();
        let (sample_rate, channels) = decoder.decode_into(&mut body);
        assert_eq!((sample_rate, channels), (48_000, 2));
        assert!(!body.is_empty(), "body-sized stream must decode before EOF");
    }

    #[test]
    fn stream_publication_sends_ready_once_before_samples_and_handles_empty_eof() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (tx, mut rx) = stream_channel();
            let mut pool = PcmPool::new();
            let mut publication = StreamPublication::default();
            let state = StreamingState::new();

            let mut first = pool.take();
            first.buffer_mut().extend_from_slice(&[0.1, 0.2]);
            assert!(
                publish_stream_chunk(&tx, &state, first, 48_000, 1, &mut publication)
                    .await
                    .unwrap()
            );
            let mut second = pool.take();
            second.buffer_mut().extend_from_slice(&[0.3]);
            assert!(
                publish_stream_chunk(&tx, &state, second, 48_000, 1, &mut publication)
                    .await
                    .unwrap()
            );

            assert!(matches!(
                rx.recv().await,
                Some(StreamMsg::Ready {
                    sample_rate: 48_000,
                    channels: 1,
                    ..
                })
            ));
            assert!(matches!(
                rx.recv().await,
                Some(StreamMsg::Samples { pcm: _, .. })
            ));
            assert!(matches!(
                rx.recv().await,
                Some(StreamMsg::Samples { pcm: _, .. })
            ));
            assert_eq!(publication.total_samples, 3);
            assert!(publication.ready_sent);

            let empty = pool.take();
            assert!(
                publish_stream_chunk(&tx, &state, empty, 0, 0, &mut publication)
                    .await
                    .unwrap()
            );
            assert!(rx.try_recv().is_err(), "empty EOF must publish no messages");
        });
    }

    #[test]
    fn production_streaming_has_no_per_track_client_construction() {
        let production = include_str!("streaming.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert!(!production.contains("create_policy_http_client("));
        assert!(!production.contains("HttpClientBuilder::new("));
    }
}

#[cfg(test)]
mod timeline_publication_tests {
    use super::*;
    fn pcm(pool: &mut PcmPool, n: usize) -> PcmChunk {
        let mut chunk = pool.take();
        chunk.buffer_mut().resize(n, 0.25);
        chunk
    }
    #[tokio::test]
    async fn only_a_proven_output_endpoint_can_publish_an_empty_traversal() {
        let timeline = StreamTimeline::new(100, 44100, 16000, 2).unwrap();
        for start in [None, Some(0), Some(timeline.output_frames())] {
            let state = StreamingState::new();
            let (tx, mut rx) = stream_channel();
            let mut pool = PcmPool::new();
            let mut publication = StreamPublication::default();
            if let Some(start) = start {
                publication.bind_timeline(timeline, start).unwrap();
            }
            publish_stream_chunk(&tx, &state, pcm(&mut pool, 0), 16000, 2, &mut publication)
                .await
                .unwrap();
            assert!(matches!(rx.try_recv(), Ok(StreamMsg::Ready { .. })));
            assert!(publication.finish(&tx, &state, false, 0).await.is_err());
            assert!(publication.finish(&tx, &state, true, 1).await.is_err());
            let complete = publication.finish(&tx, &state, true, 0).await;
            if start == Some(timeline.output_frames()) {
                assert!(complete.unwrap());
                assert!(matches!(rx.try_recv(), Ok(StreamMsg::Done { .. })));
                assert!(publication.finish(&tx, &state, true, 0).await.is_err());
            } else {
                assert!(complete.is_err());
                assert!(rx.try_recv().is_err());
            }
        }
    }
    #[tokio::test]
    async fn clocked_publication_rejects_short_excess_and_changed_format() {
        let timeline = StreamTimeline::new(100, 44100, 16000, 2).unwrap();
        assert_eq!(timeline.output_frames(), 37);
        let state = StreamingState::new();
        let (tx, mut rx) = stream_channel();
        let mut pool = PcmPool::new();
        let mut publication = StreamPublication::default();
        publication.bind_timeline(timeline, 0).unwrap();
        assert!(publication.bind_timeline(timeline, 38).is_err());
        let foreign_format = StreamTimeline::new(37, 48000, 48000, 2).unwrap();
        assert!(publication.bind_timeline(foreign_format, 0).is_err());
        assert!(
            publication
                .prepare_chunk(pcm(&mut pool, 0), 48000, 2)
                .is_err()
        );
        publish_stream_chunk(&tx, &state, pcm(&mut pool, 72), 16000, 2, &mut publication)
            .await
            .unwrap();
        assert!(matches!(rx.try_recv(), Ok(StreamMsg::Ready { .. })));
        assert!(matches!(rx.try_recv(), Ok(StreamMsg::Samples { .. })));
        assert!(publication.finish(&tx, &state, true, 0).await.is_err());
        assert!(
            publication
                .prepare_chunk(pcm(&mut pool, 4), 16000, 2)
                .is_err()
        );
        publish_stream_chunk(&tx, &state, pcm(&mut pool, 2), 16000, 2, &mut publication)
            .await
            .unwrap();
        assert!(matches!(rx.try_recv(), Ok(StreamMsg::Samples { .. })));
        assert!(publication.finish(&tx, &state, true, 0).await.unwrap());
    }
    #[test]
    fn a_new_revision_resets_its_length_and_invalid_binding_preserves_the_ledger() {
        let state = StreamingState::new();
        let timeline = StreamTimeline::new(100, 44100, 16000, 2).unwrap();
        let mut publication = StreamPublication::default();
        publication.bind_timeline(timeline, 37).unwrap();
        assert!(publication.has_timeline());
        assert!(publication.bind_timeline(timeline, 0).is_err());
        assert_eq!(publication.expected_samples, Some(0));
        publication
            .begin(StreamRevision::INITIAL.checked_next().unwrap(), &state)
            .unwrap();
        assert!(!publication.has_timeline());
        publication.bind_timeline(timeline, 0).unwrap();
        assert_eq!(publication.expected_samples, Some(74));
        let overflow = StreamTimeline::new(u64::MAX, 48000, 48000, 2).unwrap();
        assert!(publication.bind_timeline(overflow, 0).is_err());
        assert_eq!(publication.expected_samples, Some(74));
    }
}

#[cfg(test)]
mod revision_tests {
    use super::*;

    fn pcm(pool: &mut PcmPool, values: &[f32]) -> PcmChunk {
        let mut pcm = pool.take();
        pcm.buffer_mut().extend_from_slice(values);
        pcm
    }

    #[test]
    fn revision_exhaustion_does_not_wrap() {
        let last = StreamRevision(u64::MAX - 1).checked_next().unwrap();
        assert_eq!(last, StreamRevision(u64::MAX));
        assert_eq!(
            last.checked_next().unwrap_err().code,
            ErrorCode::InputSaturated
        );
    }

    #[tokio::test]
    async fn every_traversal_has_its_own_ready_pcm_and_exactly_one_done() {
        let (tx, mut rx) = stream_channel();
        let state = StreamingState::new();
        let mut pool = PcmPool::new();
        let mut publication = StreamPublication::default();
        for index in 0..3 {
            let revision = StreamRevision(index);
            if index != 0 {
                publication.begin(revision, &state).unwrap();
            }
            assert!(!state.download_complete.load(Ordering::Acquire));
            assert!(
                publish_stream_chunk(
                    &tx,
                    &state,
                    pcm(&mut pool, &[1.0, 2.0]),
                    8000,
                    2,
                    &mut publication
                )
                .await
                .unwrap()
            );
            assert!(publication.finish(&tx, &state, true, 0).await.unwrap());
            assert!(state.download_complete.load(Ordering::Acquire));
            assert!(
                matches!(rx.try_recv(), Ok(StreamMsg::Ready { revision: r, .. }) if r == revision)
            );
            assert!(
                matches!(rx.try_recv(), Ok(StreamMsg::Samples { revision: r, pcm }) if r == revision && *pcm == [1.0, 2.0])
            );
            assert!(matches!(rx.try_recv(), Ok(StreamMsg::Done { revision: r }) if r == revision));
            assert!(publication.finish(&tx, &state, true, 0).await.is_err());
            assert!(
                publish_stream_chunk(&tx, &state, pcm(&mut pool, &[]), 0, 0, &mut publication)
                    .await
                    .is_err()
            );
            assert!(rx.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn rejected_revision_and_changed_format_preserve_publication_state() {
        let (tx, mut rx) = stream_channel();
        let state = StreamingState::new();
        let mut pool = PcmPool::new();
        let mut p = StreamPublication::default();
        assert!(
            publish_stream_chunk(&tx, &state, pcm(&mut pool, &[1.0, 2.0]), 8000, 2, &mut p)
                .await
                .unwrap()
        );
        while rx.try_recv().is_ok() {}
        let next = StreamRevision::INITIAL.checked_next().unwrap();
        p.begin(next, &state).unwrap();
        assert!(p.begin(StreamRevision::INITIAL, &state).is_err());
        assert!(p.begin(next, &state).is_err());
        assert_eq!(p.revision, next);
        assert_eq!(p.total_samples, 0);
        assert!(!p.ready_sent);
        assert!(
            publish_stream_chunk(&tx, &state, pcm(&mut pool, &[1.0, 2.0]), 16000, 2, &mut p)
                .await
                .is_err()
        );
        assert!(rx.try_recv().is_err());
        assert!(
            publish_stream_chunk(&tx, &state, pcm(&mut pool, &[3.0, 4.0]), 8000, 2, &mut p)
                .await
                .unwrap()
        );
        assert_eq!(p.total_samples, 2);
    }

    #[tokio::test]
    async fn overflow_and_incomplete_eof_do_not_publish_success() {
        let (tx, mut rx) = stream_channel();
        let state = StreamingState::new();
        let mut pool = PcmPool::new();
        let mut p = StreamPublication::default();
        assert!(p.finish(&tx, &state, true, 0).await.is_err());
        assert!(
            publish_stream_chunk(&tx, &state, pcm(&mut pool, &[1.0, 2.0]), 8000, 2, &mut p)
                .await
                .unwrap()
        );
        while rx.try_recv().is_ok() {}
        assert!(p.finish(&tx, &state, false, 0).await.is_err());
        assert!(p.finish(&tx, &state, true, 1).await.is_err());
        p.total_samples = u64::MAX;
        assert!(
            publish_stream_chunk(&tx, &state, pcm(&mut pool, &[1.0, 2.0]), 8000, 2, &mut p)
                .await
                .is_err()
        );
        assert!(rx.try_recv().is_err());
        assert!(!state.download_complete.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn cancelling_a_blocked_new_traversal_recycles_pcm_without_completing_it() {
        let (tx, mut rx) = stream_channel();
        let state = StreamingState::new();
        let mut pool = PcmPool::new();
        for _ in 0..STREAM_CHANNEL_CAPACITY {
            assert!(
                tx.try_send(StreamMsg::Done {
                    revision: StreamRevision::INITIAL
                })
                .is_ok()
            );
        }
        let mut p = StreamPublication::default();
        p.begin(StreamRevision::INITIAL.checked_next().unwrap(), &state)
            .unwrap();
        let mut chunk = pcm(&mut pool, &[1.0, 2.0]);
        let pointer = chunk.buffer_mut().as_ptr();
        let operation = publish_stream_chunk(&tx, &state, chunk, 8000, 2, &mut p);
        tokio::pin!(operation);
        assert!(
            tokio::time::timeout(Duration::from_millis(5), &mut operation)
                .await
                .is_err()
        );
        state.cancel();
        assert!(
            !tokio::time::timeout(Duration::from_secs(1), operation)
                .await
                .unwrap()
                .unwrap()
        );
        // Drop the pinned future before inspecting the mutably borrowed ledger.
        while rx.try_recv().is_ok() {}
        assert_eq!(pool.take().buffer_mut().as_ptr(), pointer);
        assert!(!state.download_complete.load(Ordering::Acquire));
    }
}

#[path = "stream_pcm_buffer.rs"]
mod pcm_buffer;
#[cfg(test)]
#[path = "stream_pcm_retirement_tests.rs"]
mod pcm_retirement_tests;
