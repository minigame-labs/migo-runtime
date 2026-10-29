//! Streaming audio download and decode for edge-download-edge-play
//!
//! Downloads audio from URL in chunks and decodes progressively.
//! Only MP3 is supported for streaming as it can be decoded frame-by-frame.
//!
//! Uses a shared tokio runtime for all download tasks to avoid per-stream overhead.

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use shared::error::{EngineError, EngineResult, ErrorCode};
use tokio::sync::{Notify, mpsc};

use crate::decoder::mp3::{
    MAX_SAMPLES_PER_FRAME, Mp3FrameDecoder, Mp3Step, STREAM_LOOKAHEAD_BYTES, append_as_f32,
    first_frame_at_front,
};
#[cfg(test)]
use crate::off_worker::OffWorker;

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
    buffer: Vec<f32>,
    home: mpsc::Sender<Vec<f32>>,
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
        // the stream is gone; either way this buffer is simply released. That
        // matters because the common caller is the audio thread.
        let _ = self.home.try_send(buffer);
    }
}

/// The producer's half of the loan: hands out chunks, takes back what returns.
pub(crate) struct PcmPool {
    home: mpsc::Sender<Vec<f32>>,
    returns: mpsc::Receiver<Vec<f32>>,
}

impl PcmPool {
    pub(crate) fn new() -> Self {
        // One slot more than can be in flight, so a healthy stream never fails a
        // return for want of room.
        let (home, returns) = mpsc::channel(STREAM_CHANNEL_CAPACITY + 1);
        Self { home, returns }
    }

    pub(crate) fn take(&mut self) -> PcmChunk {
        let buffer = self
            .returns
            .try_recv()
            .unwrap_or_else(|_| Vec::with_capacity(PCM_CHUNK_SAMPLES));
        PcmChunk {
            buffer,
            home: self.home.clone(),
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
    /// Event-driven cancellation edge for tasks blocked on network I/O.
    cancel_notify: Notify,
    /// A reusable producer keeps its source and command receiver alive at EOF.
    retain_after_eof: bool,
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

    pub(crate) fn retains_source(&self) -> bool {
        self.retain_after_eof
    }

    pub fn cancel(&self) {
        if !self.cancelled.swap(true, Ordering::AcqRel) {
            self.cancel_notify.notify_waiters();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
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
            cancel_notify: Notify::new(),
            retain_after_eof: false,
        }
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

fn get_stream_runtime() -> &'static tokio::runtime::Runtime {
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

    get_stream_runtime().spawn(crate::streaming_transport::run(
        cpu,
        client,
        url,
        tx,
        state,
        move || Mp3StreamDecoder::new(target_sample_rate),
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
    finished: bool,
}

impl StreamPublication {
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
        if self.finished || !eof || remaining != 0 || !self.ready_sent || self.total_samples == 0 {
            return Err(EngineError::from_detail(
                ErrorCode::IoError,
                "stream decoder finished without complete audio input",
            ));
        }
        if !crate::streaming_transport::send(
            tx,
            state,
            StreamMsg::Done {
                revision: self.revision,
            },
        )
        .await
        {
            return Ok(false);
        }
        self.finished = true;
        state.download_complete.store(true, Ordering::Release);
        Ok(true)
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
    if publication.finished {
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
        if publication.format.is_some_and(|old| old != format) {
            return Err(EngineError::from_detail(
                ErrorCode::IoError,
                "stream PCM format changed",
            ));
        }
        publication.format = Some(format);
    }
    let total = publication
        .total_samples
        .checked_add(pcm.len() as u64)
        .ok_or_else(|| {
            EngineError::from_detail(ErrorCode::Internal, "stream PCM counter overflow")
        })?;
    if !publication.ready_sent && has_format {
        if !crate::streaming_transport::send(
            tx,
            state,
            StreamMsg::Ready {
                revision: publication.revision,
                sample_rate,
                channels,
            },
        )
        .await
        {
            return Ok(false);
        }
        publication.ready_sent = true;
    }
    if !pcm.is_empty() {
        if !crate::streaming_transport::send(
            tx,
            state,
            StreamMsg::Samples {
                revision: publication.revision,
                pcm,
            },
        )
        .await
        {
            return Ok(false);
        }
        publication.total_samples = total;
    }
    Ok(true)
}

/// Bytes of undecoded MP3 the receive buffer starts out able to hold.
///
/// Enough for a typical network chunk plus the lookahead the decoder keeps
/// behind its cursor, so the buffer settles at one allocation for the stream.
const RECEIVE_BUFFER_BYTES: usize = 64 * 1024;

/// MP3 streaming decoder.
///
/// Everything here is owned once and reused: the minimp3 decoder (whose state a
/// frame's main data is entitled to reach back into), the receive buffer, and
/// the scratch a frame is converted through on the way to the resampler. What a
/// chunk costs is a memcpy of its bytes and the samples it produces, and nothing
/// else.
struct Mp3StreamDecoder {
    frames: Mp3FrameDecoder,
    /// Bytes received and not yet decoded.
    buffer: Vec<u8>,
    /// Detected sample rate
    sample_rate: u32,
    /// Detected channels
    channels: u32,
    /// Target sample rate for resampling
    target_sample_rate: u32,
    /// Resampler state (if needed)
    resampler: Option<crate::resampler::StreamResampler>,
    /// One frame as `f32`, reused. Only the resampling path needs it; without a
    /// resampler a frame goes straight into the caller's buffer.
    scratch: Vec<f32>,
}

impl crate::streaming_transport::Decoder for Mp3StreamDecoder {
    fn step(
        &mut self,
        input: &[u8],
        eof: bool,
        pcm: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<crate::streaming_transport::Step> {
        use crate::streaming_transport::{Progress, Step};
        if cancel.load(Ordering::Acquire) {
            return Ok(Step {
                consumed: 0,
                progress: Progress::Cancelled,
                sample_rate: 0,
                channels: 0,
            });
        }
        // Bound the legacy fallback too. A single HTTP chunk may contain an
        // entire file; its size must not determine a blocking-pool quantum or
        // grow retained encoded bytes without limit.
        let consumed = input
            .len()
            .min(4096)
            .min(RECEIVE_BUFFER_BYTES - self.buffer.len());
        self.push_data(&input[..consumed]);
        let (sample_rate, channels) = if eof {
            self.flush_into(pcm)
        } else {
            self.decode_into(pcm)
        };
        Ok(Step {
            consumed,
            progress: if eof {
                Progress::Finished
            } else {
                Progress::NeedInput
            },
            sample_rate,
            channels,
        })
    }
}

impl Mp3StreamDecoder {
    fn new(target_sample_rate: u32) -> Self {
        Self {
            frames: Mp3FrameDecoder::new(),
            buffer: Vec::with_capacity(RECEIVE_BUFFER_BYTES),
            sample_rate: 0,
            channels: 0,
            target_sample_rate,
            resampler: None,
            scratch: Vec::with_capacity(MAX_SAMPLES_PER_FRAME),
        }
    }

    fn push_data(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    /// Decode what the buffer can spare, appending to `out`.
    ///
    /// Leaves a lookahead behind the cursor so minimp3 can confirm the next
    /// frame's header and keep its state; see [`STREAM_LOOKAHEAD_BYTES`]. What
    /// the lookahead strands at the end of the stream is [`Self::flush_into`]'s
    /// problem, and it has a way past it.
    fn decode_into(&mut self, out: &mut Vec<f32>) -> (u32, u32) {
        self.decode_with_lookahead(STREAM_LOOKAHEAD_BYTES, out)
    }

    /// Decode everything that is left, for a stream that has ended.
    ///
    /// Isolation first, and **the order is the fix, not a preference.** An
    /// in-place decode that fails resets minimp3 — and what it resets is the bit
    /// reservoir the frame it just failed on is entitled to. Trying in place
    /// first therefore destroys the state needed to recover the very frame it was
    /// reaching for; measured, that cost the last frame of every tagged stream
    /// and all six frames of a short one.
    ///
    /// The in-place pass is kept as a fallback for the one thing isolation cannot
    /// do: get past leading bytes that are not audio and are larger than a frame,
    /// which an ID3v2 tag routinely is. It runs only when isolation could not move
    /// at all, so a failed attempt cannot cost a frame that was recoverable.
    fn flush_into(&mut self, out: &mut Vec<f32>) -> (u32, u32) {
        let before = self.buffer.len();
        self.isolate_remaining_frames(out);

        if !self.buffer.is_empty() && self.buffer.len() == before {
            self.decode_with_lookahead(0, out);
            self.isolate_remaining_frames(out);
        }

        // The resampler defers any output frame whose interpolation neighbour has
        // not arrived. At the end of a stream it never will, so without this the
        // tail of every resampled track was dropped -- a step discontinuity at a
        // loop seam, heard as a click on each repeat.
        if let Some(resampler) = self.resampler.as_mut() {
            resampler.flush_into(out);
        }

        (self.output_sample_rate(), self.channels)
    }

    #[inline]
    fn output_sample_rate(&self) -> u32 {
        if self.resampler.is_some() {
            self.target_sample_rate
        } else {
            self.sample_rate
        }
    }

    /// Decode what is left one isolated frame at a time.
    ///
    /// minimp3 rejects a frame it cannot chain to a successor, so a stream ending
    /// in an ID3v1 tag strands its final frame — and a stream shorter than the
    /// lookahead, which decodes nothing until the flush and therefore arrives
    /// here with a decoder that has never run, strands *every* frame it has. Both
    /// were measured: the second was a short remote clip decoding to silence
    /// outright.
    ///
    /// Handing the decoder exactly one frame is what gets past it, and the length
    /// comes from a throwaway decoder rather than from the real one, whose bit
    /// reservoir a rejected probe would reset. The real decoder then sees a buffer
    /// that is exactly one frame, which is its state-preserving fast path.
    fn isolate_remaining_frames(&mut self, out: &mut Vec<f32>) {
        let mut probe: Option<Mp3FrameDecoder> = None;
        let mut previous_length: Option<usize> = None;

        while !self.buffer.is_empty() {
            let probe = probe.get_or_insert_with(Mp3FrameDecoder::new);
            let Some((skip, length)) = first_frame_at_front(probe, &self.buffer, previous_length)
            else {
                break;
            };
            // Whatever sits in front of the frame goes first: the rule that lets a
            // lone frame through wants it at offset zero.
            if skip > 0 {
                self.buffer.drain(..skip);
                continue;
            }
            previous_length = Some(length);

            match self.decode_step(0, length, out) {
                Some(consumed) if consumed > 0 => {
                    self.buffer.drain(..consumed);
                }
                // A whole frame that gave nothing -- its main data in a reservoir
                // this stream never had -- is that frame lost, as in a whole-file
                // decode. Stopping here left it to the in-place pass, which
                // decoded it again over a reservoir now holding its own bytes.
                _ => {
                    self.buffer.drain(..length);
                }
            }
        }
    }

    fn decode_with_lookahead(&mut self, lookahead: usize, out: &mut Vec<f32>) -> (u32, u32) {
        let mut pos = 0usize;

        while self.buffer.len().saturating_sub(pos) > lookahead {
            let take = self.buffer.len() - pos;
            match self.decode_step(pos, take, out) {
                Some(consumed) if consumed > 0 => pos += consumed,
                _ => break,
            }
        }

        if pos > 0 {
            self.buffer.drain(..pos);
        }

        (self.output_sample_rate(), self.channels)
    }

    /// One decode against `buffer[pos..pos + take]`, appending any frame it
    /// produced. Returns the bytes it got past, or `None` for no progress.
    ///
    /// The two callers differ only in how much of the buffer they offer, which is
    /// the whole of the difference between decoding in place and isolating a
    /// frame — so they share this and cannot drift apart in how a frame is
    /// adopted.
    fn decode_step(&mut self, pos: usize, take: usize, out: &mut Vec<f32>) -> Option<usize> {
        match self.frames.decode(&self.buffer[pos..pos + take]) {
            Mp3Step::NeedMoreData => None,
            Mp3Step::Skipped(skipped) => Some(skipped),
            Mp3Step::Frame {
                pcm,
                sample_rate,
                channels,
                consumed,
            } => {
                if self.sample_rate == 0 {
                    self.sample_rate = sample_rate;
                    self.channels = channels;

                    if sample_rate != self.target_sample_rate {
                        self.resampler = Some(crate::resampler::StreamResampler::new(
                            sample_rate,
                            self.target_sample_rate,
                            channels,
                        ));
                    }
                }

                match self.resampler.as_mut() {
                    Some(resampler) => {
                        self.scratch.clear();
                        append_as_f32(pcm, &mut self.scratch);
                        resampler.process_into(&self.scratch, out);
                    }
                    None => append_as_f32(pcm, out),
                }

                Some(consumed)
            }
        }
    }
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
                            decoder.buffer.len()
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
