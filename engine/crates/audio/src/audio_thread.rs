use std::collections::HashMap;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use shared::audio_channel::{
    AUDIO_COMMANDS_PER_DRAIN, AudioCommandReceiver, AudioCommandSender, channel as audio_channel,
};
use shared::audio_resources::AudioSnapshot;
use shared::channel::ThreadWakeup;
use shared::error::{EngineError, EngineResult, ErrorCode};
use shared::op_state::HostTx;
use shared::protocol::audio_cmd::{
    AudioBufferInfo, AudioCmd, AudioContextId, AudioContextState, AudioNodeId, AudioResp,
    InnerAudioEvent, InnerAudioId, InnerAudioInfo, InnerAudioState,
};
use shared::protocol::host_cmd::HostCommand;
use tracing::{error, info, warn};

/// Best-effort thread join with a timeout.  Falls back to detaching the
/// thread if it does not finish within `timeout`.
fn join_with_timeout(handle: thread::JoinHandle<()>, timeout: Duration, label: &str) {
    let caller = thread::current();
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done2 = done.clone();
    let _waiter = thread::spawn(move || {
        let _ = handle.join();
        done2.store(true, std::sync::atomic::Ordering::Release);
        caller.unpark();
    });
    thread::park_timeout(timeout);
    if !done.load(std::sync::atomic::Ordering::Acquire) {
        warn!(
            "{} did not shut down within {:?}, detaching",
            label, timeout
        );
    }
}

fn join_all_with_timeout(
    handles: impl IntoIterator<Item = thread::JoinHandle<()>>,
    total_timeout: Duration,
    label: &str,
) {
    let deadline = Instant::now() + total_timeout;
    for (i, handle) in handles.into_iter().enumerate() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        join_with_timeout(handle, remaining, &format!("{}-{}", label, i));
    }
}

use crate::decoder::DecodedAudio;

/// Result of an off-thread decode+resample operation.
///
/// The heavy work (decoding compressed audio, resampling) runs on a
/// short-lived `std::thread`. When finished, the result is sent back
/// to the audio loop via a [`std_mpsc::Receiver`] so the audio thread
/// can integrate it without blocking.
enum DecodeResult {
    /// Completed decode for `AudioCmd::DecodeAudioData`.
    AudioBuffer {
        ctx_id: AudioContextId,
        result: EngineResult<DecodedAudio>,
        resp: AudioResp<AudioBufferInfo>,
    },
    /// Completed decode for `AudioCmd::InnerAudioLoad`.
    InnerAudio {
        id: InnerAudioId,
        result: EngineResult<DecodedAudio>,
        resp: AudioResp<InnerAudioInfo>,
    },
}

// ---------------------------------------------------------------------------
// Decode thread pool
// ---------------------------------------------------------------------------

/// Payload for a decode job submitted to the pool.
enum DecodeJob {
    AudioBuffer {
        ctx_id: AudioContextId,
        data: std::sync::Arc<Vec<u8>>,
        resp: AudioResp<AudioBufferInfo>,
    },
    InnerAudio {
        id: InnerAudioId,
        data: Vec<u8>,
        resp: AudioResp<InnerAudioInfo>,
    },
}

/// Number of persistent decode worker threads.
///
/// 2 is a good default for mobile: enough parallelism for startup bursts
/// (30 sound effects decode in ~225 ms instead of ~450 ms with 1 worker)
/// without hogging CPU cores needed for rendering and JS.
const DECODE_POOL_SIZE: usize = 2;

/// Maximum number of encoded decode jobs waiting behind the workers.
const DECODE_JOB_QUEUE_CAPACITY: usize = 16;

/// Maximum number of completed decodes waiting for the audio thread.
const DECODE_RESULT_QUEUE_CAPACITY: usize = 8;

/// Encoded bytes retained by jobs waiting behind the decode workers.
const MAX_DECODE_JOB_QUEUED_BYTES: usize = 64 * 1024 * 1024;

/// Decoded PCM bytes retained while the audio thread is catching up.
const MAX_DECODE_RESULT_QUEUED_BYTES: usize = 64 * 1024 * 1024;
/// One decode may retain up to 16 MiB encoded input and two 64 MiB PCM-sized
/// allocations while decoding/resampling.  This process-wide budget permits
/// two such jobs, regardless of how many runtimes create audio pools.
const MAX_DECODE_IN_FLIGHT_BYTES: usize = 288 * 1024 * 1024;
const DECODE_IN_FLIGHT_RESERVATION_BYTES: usize = 144 * 1024 * 1024;

const DECODE_JOB_QUEUE_FULL_DETAIL: &str = "audio decode job queue is full";
const DECODE_JOB_BYTE_LIMIT_DETAIL: &str = "audio decode job queue byte limit exceeded";
const DECODE_RESULT_QUEUE_FULL_DETAIL: &str = "audio decode result queue is full";
const DECODE_RESULT_BYTE_LIMIT_DETAIL: &str = "audio decode result queue byte limit exceeded";
const DECODE_QUEUE_DISCONNECTED_DETAIL: &str = "audio decode queue is disconnected";

trait DecodeQueuePayload {
    fn queued_bytes(&self) -> usize;
}

struct DecodeInFlightUsage {
    bytes: std::sync::atomic::AtomicUsize,
    max_bytes: usize,
    reservation_bytes: usize,
}

struct DecodeInFlightPermit {
    usage: Arc<DecodeInFlightUsage>,
    bytes: usize,
}

impl Drop for DecodeInFlightPermit {
    fn drop(&mut self) {
        self.usage
            .bytes
            .fetch_sub(self.bytes, std::sync::atomic::Ordering::AcqRel);
    }
}

impl DecodeInFlightUsage {
    fn new(max_bytes: usize, reservation_bytes: usize) -> Self {
        assert!(reservation_bytes > 0, "decode reservation must be non-zero");
        Self {
            bytes: std::sync::atomic::AtomicUsize::new(0),
            max_bytes,
            reservation_bytes,
        }
    }

    fn try_reserve(self: &Arc<Self>) -> Option<DecodeInFlightPermit> {
        self.bytes
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |used| {
                    used.checked_add(self.reservation_bytes)
                        .filter(|total| *total <= self.max_bytes)
                },
            )
            .ok()?;
        Some(DecodeInFlightPermit {
            usage: Arc::clone(self),
            bytes: self.reservation_bytes,
        })
    }
}

fn process_decode_in_flight_budget() -> Arc<DecodeInFlightUsage> {
    static PROCESS_DECODE_IN_FLIGHT_BUDGET: OnceLock<Arc<DecodeInFlightUsage>> = OnceLock::new();

    Arc::clone(PROCESS_DECODE_IN_FLIGHT_BUDGET.get_or_init(|| {
        Arc::new(DecodeInFlightUsage::new(
            MAX_DECODE_IN_FLIGHT_BYTES,
            DECODE_IN_FLIGHT_RESERVATION_BYTES,
        ))
    }))
}

impl DecodeQueuePayload for DecodeJob {
    fn queued_bytes(&self) -> usize {
        match self {
            Self::AudioBuffer { data, .. } => data.capacity(),
            Self::InnerAudio { data, .. } => data.capacity(),
        }
    }
}

impl DecodeQueuePayload for DecodeResult {
    fn queued_bytes(&self) -> usize {
        let result = match self {
            Self::AudioBuffer { result, .. } | Self::InnerAudio { result, .. } => result,
        };
        result.as_ref().map_or(0, |decoded| {
            decoded
                .samples
                .capacity()
                .saturating_mul(std::mem::size_of::<f32>())
        })
    }
}

struct DecodeQueueUsage {
    max_items: usize,
    max_bytes: usize,
    closed: std::sync::atomic::AtomicBool,
    items: std::sync::atomic::AtomicUsize,
    bytes: std::sync::atomic::AtomicUsize,
}

struct DecodeQueuePermit {
    usage: Arc<DecodeQueueUsage>,
    bytes: usize,
}

impl Drop for DecodeQueuePermit {
    fn drop(&mut self) {
        self.usage
            .bytes
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |used| Some(used.saturating_sub(self.bytes)),
            )
            .ok();
        self.usage
            .items
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |items| Some(items.saturating_sub(1)),
            )
            .ok();
    }
}

enum DecodeQueueReserveError {
    Full,
    ByteLimit,
    Disconnected,
}

impl DecodeQueueUsage {
    fn try_reserve(
        self: &Arc<Self>,
        bytes: usize,
    ) -> Result<DecodeQueuePermit, DecodeQueueReserveError> {
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            return Err(DecodeQueueReserveError::Disconnected);
        }
        self.items
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |items| (items < self.max_items).then_some(items + 1),
            )
            .map_err(|_| DecodeQueueReserveError::Full)?;

        if self
            .bytes
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |used| {
                    used.checked_add(bytes)
                        .filter(|total| *total <= self.max_bytes)
                },
            )
            .is_err()
        {
            self.items
                .fetch_update(
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                    |items| Some(items.saturating_sub(1)),
                )
                .ok();
            return Err(DecodeQueueReserveError::ByteLimit);
        }

        let permit = DecodeQueuePermit {
            usage: Arc::clone(self),
            bytes,
        };
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            drop(permit);
            return Err(DecodeQueueReserveError::Disconnected);
        }
        Ok(permit)
    }
}

struct DecodeQueueEntry<T> {
    value: T,
    _permit: DecodeQueuePermit,
}

impl<T> DecodeQueueEntry<T> {
    fn into_value(self) -> T {
        let Self { value, _permit } = self;
        drop(_permit);
        value
    }
}

struct DecodeQueueSender<T> {
    tx: std_mpsc::SyncSender<DecodeQueueEntry<T>>,
    usage: Arc<DecodeQueueUsage>,
}

impl<T> Clone for DecodeQueueSender<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            usage: Arc::clone(&self.usage),
        }
    }
}

struct DecodeQueueReceiver<T> {
    rx: std_mpsc::Receiver<DecodeQueueEntry<T>>,
    usage: Arc<DecodeQueueUsage>,
}

impl<T> Drop for DecodeQueueReceiver<T> {
    fn drop(&mut self) {
        self.usage
            .closed
            .store(true, std::sync::atomic::Ordering::Release);
        self.usage
            .items
            .store(0, std::sync::atomic::Ordering::Release);
        self.usage
            .bytes
            .store(0, std::sync::atomic::Ordering::Release);
    }
}

enum DecodeQueueSendError<T> {
    Full(T),
    ByteLimit(T),
    Disconnected(T),
}

impl<T> std::fmt::Debug for DecodeQueueSendError<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Full(_) => "Full(..)",
            Self::ByteLimit(_) => "ByteLimit(..)",
            Self::Disconnected(_) => "Disconnected(..)",
        })
    }
}

impl<T: DecodeQueuePayload> DecodeQueueSender<T> {
    fn try_send(&self, value: T) -> Result<(), DecodeQueueSendError<T>> {
        let permit = match self.usage.try_reserve(value.queued_bytes()) {
            Ok(permit) => permit,
            Err(DecodeQueueReserveError::Full) => {
                return Err(DecodeQueueSendError::Full(value));
            }
            Err(DecodeQueueReserveError::ByteLimit) => {
                return Err(DecodeQueueSendError::ByteLimit(value));
            }
            Err(DecodeQueueReserveError::Disconnected) => {
                return Err(DecodeQueueSendError::Disconnected(value));
            }
        };
        match self.tx.try_send(DecodeQueueEntry {
            value,
            _permit: permit,
        }) {
            Ok(()) => Ok(()),
            Err(std_mpsc::TrySendError::Full(entry)) => {
                Err(DecodeQueueSendError::Full(entry.into_value()))
            }
            Err(std_mpsc::TrySendError::Disconnected(entry)) => {
                Err(DecodeQueueSendError::Disconnected(entry.into_value()))
            }
        }
    }
}

impl<T> DecodeQueueReceiver<T> {
    fn recv(&self) -> Result<T, std_mpsc::RecvError> {
        self.rx.recv().map(DecodeQueueEntry::into_value)
    }

    fn try_recv(&self) -> Result<T, std_mpsc::TryRecvError> {
        self.rx.try_recv().map(DecodeQueueEntry::into_value)
    }

    #[cfg(test)]
    fn recv_timeout(&self, timeout: Duration) -> Result<T, std_mpsc::RecvTimeoutError> {
        self.rx
            .recv_timeout(timeout)
            .map(DecodeQueueEntry::into_value)
    }
}

fn decode_queue<T>(
    item_capacity: usize,
    byte_capacity: usize,
) -> (DecodeQueueSender<T>, DecodeQueueReceiver<T>) {
    let (tx, rx) = std_mpsc::sync_channel(item_capacity);
    let usage = Arc::new(DecodeQueueUsage {
        max_items: item_capacity,
        max_bytes: byte_capacity,
        closed: std::sync::atomic::AtomicBool::new(false),
        items: std::sync::atomic::AtomicUsize::new(0),
        bytes: std::sync::atomic::AtomicUsize::new(0),
    });
    (
        DecodeQueueSender {
            tx,
            usage: Arc::clone(&usage),
        },
        DecodeQueueReceiver { rx, usage },
    )
}

fn reject_decode_job(job: DecodeJob, code: ErrorCode, detail: &'static str) {
    let error = EngineError::from_detail(code, detail);
    match job {
        DecodeJob::AudioBuffer { resp, .. } => {
            let _ = resp.send(Err(error));
        }
        DecodeJob::InnerAudio { resp, .. } => {
            let _ = resp.send(Err(error));
        }
    }
}

fn reject_decode_result(result: DecodeResult, code: ErrorCode, detail: &'static str) {
    let error = EngineError::from_detail(code, detail);
    match result {
        DecodeResult::AudioBuffer { resp, .. } => {
            let _ = resp.send(Err(error));
        }
        DecodeResult::InnerAudio { resp, .. } => {
            let _ = resp.send(Err(error));
        }
    }
}

fn publish_decode_result(result_tx: &DecodeQueueSender<DecodeResult>, result: DecodeResult) {
    match result_tx.try_send(result) {
        Ok(()) => {}
        Err(DecodeQueueSendError::Full(result)) => reject_decode_result(
            result,
            ErrorCode::InputSaturated,
            DECODE_RESULT_QUEUE_FULL_DETAIL,
        ),
        Err(DecodeQueueSendError::ByteLimit(result)) => reject_decode_result(
            result,
            ErrorCode::InputSaturated,
            DECODE_RESULT_BYTE_LIMIT_DETAIL,
        ),
        Err(DecodeQueueSendError::Disconnected(result)) => reject_decode_result(
            result,
            ErrorCode::Disconnected,
            DECODE_QUEUE_DISCONNECTED_DETAIL,
        ),
    }
}

/// A fixed-size thread pool for audio decode + resample work.
///
/// Workers are created once and persist for the lifetime of the audio thread,
/// avoiding per-decode thread creation/teardown overhead (~100-200 us on Android).
/// Completed results are sent back via `result_tx` and the audio thread is
/// woken immediately via `wakeup.notify()`.
struct DecodePool {
    job_tx: Option<DecodeQueueSender<DecodeJob>>,
    #[cfg(test)]
    in_flight: Arc<DecodeInFlightUsage>,
    workers: Vec<thread::JoinHandle<()>>,
}

/// Owns the decode-pool configuration without starting worker threads until
/// the first decode job arrives.
struct LazyDecodePool {
    pool: Option<DecodePool>,
    result_tx: DecodeQueueSender<DecodeResult>,
    sample_rate: u32,
    wakeup: ThreadWakeup,
}

impl LazyDecodePool {
    fn new(
        result_tx: DecodeQueueSender<DecodeResult>,
        sample_rate: u32,
        wakeup: ThreadWakeup,
    ) -> Self {
        Self {
            pool: None,
            result_tx,
            sample_rate,
            wakeup,
        }
    }

    fn submit(&mut self, job: DecodeJob) {
        let pool = self.pool.get_or_insert_with(|| {
            DecodePool::new(
                DECODE_POOL_SIZE,
                self.result_tx.clone(),
                self.sample_rate,
                self.wakeup.clone(),
            )
        });
        pool.submit(job);
    }

    #[cfg(test)]
    fn is_started(&self) -> bool {
        self.pool.is_some()
    }
}

impl DecodePool {
    fn new(
        num_workers: usize,
        result_tx: DecodeQueueSender<DecodeResult>,
        sample_rate: u32,
        wakeup: shared::channel::ThreadWakeup,
    ) -> Self {
        Self::new_with_in_flight(
            num_workers,
            result_tx,
            process_decode_in_flight_budget(),
            sample_rate,
            wakeup,
        )
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn new_with_in_flight(
        num_workers: usize,
        result_tx: DecodeQueueSender<DecodeResult>,
        in_flight: Arc<DecodeInFlightUsage>,
        sample_rate: u32,
        wakeup: shared::channel::ThreadWakeup,
    ) -> Self {
        let (job_tx, job_rx) =
            decode_queue::<DecodeJob>(DECODE_JOB_QUEUE_CAPACITY, MAX_DECODE_JOB_QUEUED_BYTES);
        // Workers share the receiver via Mutex (contention is minimal since
        // workers spend most time decoding, not waiting on the lock).
        let job_rx = Arc::new(std::sync::Mutex::new(job_rx));
        let mut workers = Vec::with_capacity(num_workers);
        for i in 0..num_workers {
            let rx = job_rx.clone();
            let tx = result_tx.clone();
            let in_flight = Arc::clone(&in_flight);
            let wake = wakeup.clone();
            let sr = sample_rate;
            let handle = thread::Builder::new()
                .name(format!("audio-decode-{}", i))
                .spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        decode_worker(rx, tx, in_flight, sr, wake);
                    }));
                    if let Err(panic_info) = result {
                        let msg = if let Some(s) = panic_info.downcast_ref::<String>() {
                            s.clone()
                        } else if let Some(s) = panic_info.downcast_ref::<&str>() {
                            s.to_string()
                        } else {
                            "Unknown panic".to_string()
                        };
                        tracing::error!("[audio-decode-{}] PANIC: {}", i, msg);
                    }
                })
                .expect("failed to spawn decode worker");
            workers.push(handle);
        }

        Self {
            job_tx: Some(job_tx),
            #[cfg(test)]
            in_flight,
            workers,
        }
    }

    /// Submit a decode job without ever blocking the audio thread.
    fn submit(&self, job: DecodeJob) {
        let Some(job_tx) = &self.job_tx else {
            reject_decode_job(
                job,
                ErrorCode::Disconnected,
                DECODE_QUEUE_DISCONNECTED_DETAIL,
            );
            return;
        };
        match job_tx.try_send(job) {
            Ok(()) => {}
            Err(DecodeQueueSendError::Full(job)) => {
                reject_decode_job(job, ErrorCode::InputSaturated, DECODE_JOB_QUEUE_FULL_DETAIL)
            }
            Err(DecodeQueueSendError::ByteLimit(job)) => {
                reject_decode_job(job, ErrorCode::InputSaturated, DECODE_JOB_BYTE_LIMIT_DETAIL)
            }
            Err(DecodeQueueSendError::Disconnected(job)) => reject_decode_job(
                job,
                ErrorCode::Disconnected,
                DECODE_QUEUE_DISCONNECTED_DETAIL,
            ),
        }
    }
}

impl Drop for DecodePool {
    fn drop(&mut self) {
        // Closing the only sender lets every worker exit after already accepted
        // jobs, without needing a shutdown message to compete for queue space.
        drop(self.job_tx.take());
        // All workers share one deadline so audio-thread shutdown stays within
        // its own three-second join budget.
        join_all_with_timeout(
            self.workers.drain(..),
            Duration::from_secs(3),
            "decode-worker",
        );
    }
}

/// Keep malformed media from killing a persistent worker and orphaning every
/// response queued behind it. Decoder panics are isolated to the current job;
/// the worker publishes a normal internal error and continues draining.
fn run_decode_with_panic_boundary(
    decode: impl FnOnce() -> EngineResult<DecodedAudio>,
) -> EngineResult<DecodedAudio> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(decode)) {
        Ok(result) => result,
        Err(panic_info) => {
            let detail = panic_info
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic_info.downcast_ref::<&str>().copied())
                .unwrap_or("unknown decoder panic");
            tracing::error!("audio decoder panicked: {detail}");
            Err(EngineError::from_detail(
                ErrorCode::Internal,
                "audio decoder failed internally",
            ))
        }
    }
}

/// Worker loop: wait for jobs, decode, send result, wake audio thread.
fn decode_worker(
    job_rx: Arc<std::sync::Mutex<DecodeQueueReceiver<DecodeJob>>>,
    result_tx: DecodeQueueSender<DecodeResult>,
    in_flight: Arc<DecodeInFlightUsage>,
    sample_rate: u32,
    wakeup: shared::channel::ThreadWakeup,
) {
    loop {
        // Lock only long enough to call recv(); the actual decode runs
        // without holding the lock so other workers can pick up jobs.
        let msg = {
            let rx = match job_rx.lock() {
                Ok(rx) => rx,
                Err(_) => break, // Mutex poisoned — exit
            };
            rx.recv()
        };

        match msg {
            Ok(job) => {
                // A decode can transiently retain its 16 MiB encoded input and
                // both an input and resampled 64 MiB PCM allocation. Claim a
                // conservative 144 MiB before invoking it so all pools together
                // cannot run more than two jobs inside the process-wide 288 MiB
                // allowance (codec-internal scratch remains separately bounded
                // by the input and decoded-sample limits).
                let Some(_in_flight_permit) = in_flight.try_reserve() else {
                    reject_decode_job(
                        job,
                        ErrorCode::InputSaturated,
                        "audio decode in-flight memory budget exceeded",
                    );
                    wakeup.notify();
                    continue;
                };
                match job {
                    DecodeJob::AudioBuffer { ctx_id, data, resp } => {
                        let result = run_decode_with_panic_boundary(|| {
                            crate::decoder::decode(&data).and_then(|decoded| {
                                crate::resampler::resample_if_needed(decoded, sample_rate)
                            })
                        });
                        publish_decode_result(
                            &result_tx,
                            DecodeResult::AudioBuffer {
                                ctx_id,
                                result,
                                resp,
                            },
                        );
                    }
                    DecodeJob::InnerAudio { id, data, resp } => {
                        let result = run_decode_with_panic_boundary(|| {
                            crate::decoder::decode(&data).and_then(|decoded| {
                                crate::resampler::resample_if_needed(decoded, sample_rate)
                            })
                        });
                        publish_decode_result(
                            &result_tx,
                            DecodeResult::InnerAudio { id, result, resp },
                        );
                    }
                }
                // Wake the audio thread so it processes the result immediately,
                // rather than waiting for the next management-loop tick.
                wakeup.notify();
            }
            Err(_) => break,
        }
    }
}

use crate::cache::GlobalAudioCache;
use crate::context::AudioContext;
use crate::inner_audio::{InnerAudioPlayer, PlaybackState};
use crate::nodes::{
    AnalyserNode, BiquadFilterNode, BiquadFilterType, ChannelMergerNode, ChannelSplitterNode,
    ConstantSourceNode, DelayNode, DistanceModel, DynamicsCompressorNode, IIRFilterNode,
    OscillatorNode, OscillatorType, OversampleType, PannerNode, PanningModel, WaveShaperNode,
};
use crate::output::AudioOutput;
use crate::power_manager::{
    AudioPowerConfig, AudioPowerManager, AudioPowerState, AudioStreamAction, AudioStreamGate,
    AudioWaitMode, audio_wait_mode,
};
use crate::streaming::{self, LazyStreamingClient, StreamingHttpClientFactory, StreamingState};

/// Reverse lookup from node_id to context_id for O(1) access.
/// This avoids iterating all contexts when looking up a node.
struct NodeContextIndex {
    node_to_ctx: HashMap<AudioNodeId, AudioContextId>,
}

impl NodeContextIndex {
    fn new() -> Self {
        Self {
            node_to_ctx: HashMap::with_capacity(64),
        }
    }

    #[inline]
    fn register(&mut self, node_id: AudioNodeId, ctx_id: AudioContextId) {
        self.node_to_ctx.insert(node_id, ctx_id);
    }

    #[inline]
    fn unregister(&mut self, node_id: AudioNodeId) {
        self.node_to_ctx.remove(&node_id);
    }

    #[inline]
    fn get_context(&self, node_id: AudioNodeId) -> Option<AudioContextId> {
        self.node_to_ctx.get(&node_id).copied()
    }

    fn clear_context(&mut self, ctx_id: AudioContextId) {
        self.node_to_ctx.retain(|_, &mut v| v != ctx_id);
    }

    fn clear_all(&mut self) {
        self.node_to_ctx.clear();
    }
}

/// Insert a new context without ever replacing a live context that happens to
/// have the same externally allocated id.
fn create_context_scoped(
    contexts: &mut HashMap<AudioContextId, AudioContext>,
    ctx_id: AudioContextId,
    sample_rate: u32,
    channels: u32,
) -> bool {
    match contexts.entry(ctx_id) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(AudioContext::new(ctx_id, sample_rate, channels));
            true
        }
        std::collections::hash_map::Entry::Occupied(_) => false,
    }
}

/// Shared context cleanup for explicit close and GC finalization. The index is
/// cleared even when the context is already gone so repeated finalizer commands
/// also purge any stale node ownership entries.
fn release_context_scoped(
    contexts: &mut HashMap<AudioContextId, AudioContext>,
    node_index: &mut NodeContextIndex,
    ctx_id: AudioContextId,
) -> bool {
    let found = if let Some(mut context) = contexts.remove(&ctx_id) {
        context.close();
        true
    } else {
        false
    };
    node_index.clear_context(ctx_id);
    found
}

/// Restart barrier for WebAudio state. All work accepted before the barrier but
/// not yet executed is dropped first, then every retained WebAudio context is
/// closed and the ownership index is cleared. The acknowledgement is published
/// last so a replacement isolate cannot race any of those old commands.
fn handle_release_all_contexts(
    startup_backlog: &mut std::vec::IntoIter<AudioCmd>,
    rx: &AudioCommandReceiver,
    contexts: &mut HashMap<AudioContextId, AudioContext>,
    node_index: &mut NodeContextIndex,
) {
    *startup_backlog = Vec::new().into_iter();
    rx.discard_data_queue();
    for (_, mut context) in contexts.drain() {
        context.close();
    }
    node_index.clear_all();
    rx.complete_release_all_contexts();
}

/// Remove only the claimed context's map reference. Missing contexts/buffers
/// are deliberately a no-op so GC finalizers can issue duplicate releases.
fn release_buffer_scoped(
    contexts: &mut HashMap<AudioContextId, AudioContext>,
    ctx_id: AudioContextId,
    buffer_id: shared::protocol::audio_cmd::AudioBufferId,
) {
    if let Some(context) = contexts.get_mut(&ctx_id) {
        context.remove_buffer(buffer_id);
    }
}

/// Mark a node unreachable from JavaScript and unregister whatever that made
/// collectible.
///
/// Scoped through the node index like every other node command, so a runtime
/// cannot release a node it does not own. Unknown ids are a no-op: this comes
/// from a GC finalizer, which may run after the context is already gone.
fn release_node_scoped(
    contexts: &mut HashMap<AudioContextId, AudioContext>,
    node_index: &mut NodeContextIndex,
    ctx_id: AudioContextId,
    node_id: AudioNodeId,
) {
    if node_index.get_context(node_id) != Some(ctx_id) {
        return;
    }
    let Some(context) = contexts.get_mut(&ctx_id) else {
        return;
    };
    for &removed in context.release_node(node_id) {
        node_index.unregister(removed);
    }
}

/// Native ownership boundary for binding and clearing a source buffer.
/// Node ids and buffer ids are context-local, so both are resolved only after
/// the claimed owner matches the node index.
fn set_buffer_scoped(
    contexts: &mut HashMap<AudioContextId, AudioContext>,
    node_index: &NodeContextIndex,
    ctx_id: AudioContextId,
    node_id: AudioNodeId,
    buffer_id: Option<shared::protocol::audio_cmd::AudioBufferId>,
) -> bool {
    if node_index.get_context(node_id) != Some(ctx_id) {
        return false;
    }
    contexts
        .get_mut(&ctx_id)
        .map(|context| context.set_buffer(node_id, buffer_id))
        .unwrap_or(false)
}

fn set_started_buffer_scoped(
    contexts: &mut HashMap<AudioContextId, AudioContext>,
    node_index: &NodeContextIndex,
    ctx_id: AudioContextId,
    node_id: AudioNodeId,
    buffer: Option<Arc<AudioSnapshot>>,
) -> bool {
    if node_index.get_context(node_id) != Some(ctx_id) {
        return false;
    }
    contexts
        .get_mut(&ctx_id)
        .map(|context| context.set_started_buffer(node_id, buffer))
        .unwrap_or(false)
}

fn start_buffer_scoped(
    contexts: &mut HashMap<AudioContextId, AudioContext>,
    node_index: &NodeContextIndex,
    ctx_id: AudioContextId,
    node_id: AudioNodeId,
    buffer: Option<Arc<AudioSnapshot>>,
    when: f64,
    offset: f64,
    duration: Option<f64>,
) -> bool {
    if node_index.get_context(node_id) != Some(ctx_id) {
        return false;
    }
    let Some(context) = contexts.get_mut(&ctx_id) else {
        return false;
    };
    // Keep the replacement and start in one audio-thread operation, so the
    // source can never render a block from the previous snapshot after this
    // command has been accepted.
    context.set_started_buffer(node_id, buffer)
        && context.start_source(node_id, when, offset, duration)
}

/// Integrate one completed WebAudio decode only while its originating runtime
/// still owns the response receiver. A closed receiver identifies stale work
/// from a dropped/restarted runtime; its decoded PCM is dropped immediately,
/// even if a newer context has since reused the same numeric id.
fn integrate_audio_buffer_decode_result(
    contexts: &mut HashMap<AudioContextId, AudioContext>,
    ctx_id: AudioContextId,
    result: EngineResult<DecodedAudio>,
    resp: AudioResp<AudioBufferInfo>,
) {
    if resp.is_closed() {
        return;
    }

    match result {
        Ok(resampled) => {
            let Some(context) = contexts.get_mut(&ctx_id) else {
                let _ = resp.send(Err(EngineError::from_detail(
                    ErrorCode::NotFound,
                    format!("AudioContext {} closed during decode", ctx_id),
                )));
                return;
            };
            let duration = resampled.duration();
            let sample_rate = resampled.sample_rate;
            let channels = resampled.channels;
            let length = resampled.frame_count() as u32;
            match context.add_buffer(resampled) {
                Ok(id) => {
                    let response = AudioBufferInfo {
                        id,
                        duration,
                        sample_rate,
                        channels,
                        length,
                    };
                    if resp.send(Ok(response)).is_err() {
                        // The receiver closed between the liveness check and
                        // send. No caller learned the id, so roll insertion back.
                        context.remove_buffer(id);
                    }
                }
                Err(error) => {
                    let _ = resp.send(Err(error));
                }
            }
        }
        Err(error) => {
            let _ = resp.send(Err(error));
        }
    }
}

/// Result of thread initialization
enum InitResult {
    Ok(thread::ThreadId),
    Err(String),
}

pub struct AudioThread {
    tx: AudioCommandSender,
    wakeup: ThreadWakeup,
    handle: Option<thread::JoinHandle<()>>,
    thread_id: thread::ThreadId,
}

impl AudioThread {
    pub fn spawn(
        host_tx: HostTx,
        http_client_factory: StreamingHttpClientFactory,
    ) -> EngineResult<Self> {
        let (tx, rx) = audio_channel();
        let (init_tx, init_rx) = std::sync::mpsc::sync_channel::<InitResult>(1);

        // Create the shared wakeup handle. A clone lives in the AudioThread
        // (and is handed to AudioSender via `sender()`), another clone goes
        // into `run_audio_thread`.
        let wakeup = ThreadWakeup::new();
        let wakeup_for_thread = wakeup.clone();

        let handle = thread::Builder::new()
            .name("Migo-AudioThread".into())
            .spawn(move || {
                // Audio thread uses Oboe's SCHED_FIFO for the callback thread;
                // the management thread itself gets Background priority.
                shared::thread_priority::set_current_thread_priority(
                    shared::thread_priority::Priority::Background,
                );
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    // Initialize audio output
                    let output = match AudioOutput::new() {
                        Ok(out) => {
                            let _ = init_tx.send(InitResult::Ok(thread::current().id()));
                            out
                        }
                        Err(e) => {
                            error!("Failed to initialize audio output: {}", e);
                            let _ = init_tx.send(InitResult::Err(e.to_string()));
                            return;
                        }
                    };

                    info!("AudioThread started");

                    // Run the audio thread loop with power management
                    run_audio_thread(
                        rx,
                        Vec::new(),
                        output,
                        host_tx,
                        wakeup_for_thread,
                        http_client_factory,
                    );

                    info!("AudioThread stopped");
                })); // end catch_unwind
                if let Err(panic_info) = result {
                    let msg = if let Some(s) = panic_info.downcast_ref::<String>() {
                        s.clone()
                    } else if let Some(s) = panic_info.downcast_ref::<&str>() {
                        s.to_string()
                    } else {
                        "Unknown panic".to_string()
                    };
                    error!("[AudioThread] PANIC: {}", msg);
                }
            })
            .map_err(|e| {
                EngineError::from_detail(
                    ErrorCode::IoError,
                    format!("Failed to spawn audio thread: {}", e),
                )
            })?;

        // Wait for initialization result
        match init_rx.recv() {
            Ok(InitResult::Ok(thread_id)) => Ok(Self {
                tx,
                wakeup,
                handle: Some(handle),
                thread_id,
            }),
            Ok(InitResult::Err(e)) => Err(EngineError::from_detail(
                ErrorCode::Internal,
                format!("Audio thread initialization failed: {}", e),
            )),
            Err(_) => Err(EngineError::from_detail(
                ErrorCode::Internal,
                "Audio thread terminated before initialization",
            )),
        }
    }

    /// Return an [`AudioSender`](shared::op_state::AudioSender) that wraps
    /// the command channel + wakeup handle. Every `.send()` on the returned
    /// sender also signals the audio thread to wake up immediately.
    #[inline]
    pub fn sender(&self) -> shared::op_state::AudioSender {
        shared::op_state::AudioSender::new(self.tx.clone(), self.wakeup.clone())
    }

    /// Spawn the audio thread using a **pre-existing** channel + wakeup.
    ///
    /// This is the lazy-init variant used by [`AudioService`]: the channel is
    /// created early (so ops can start sending commands immediately), but the
    /// thread is only spawned when the first real audio command arrives.
    ///
    /// Unlike [`spawn`], this does **not** block the caller waiting for
    /// `AudioOutput::new()` to complete. The thread initialises audio output
    /// asynchronously; commands queued before init finishes are buffered in the
    /// channel and processed once the output is ready.
    pub fn spawn_with_channel(
        tx: AudioCommandSender,
        rx: AudioCommandReceiver,
        startup_backlog: Vec<AudioCmd>,
        wakeup: ThreadWakeup,
        host_tx: HostTx,
        http_client_factory: StreamingHttpClientFactory,
    ) -> EngineResult<Self> {
        let wakeup_for_thread = wakeup.clone();

        let handle = thread::Builder::new()
            .name("Migo-AudioThread".into())
            .spawn(move || {
                shared::thread_priority::set_current_thread_priority(
                    shared::thread_priority::Priority::Background,
                );
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let output = match AudioOutput::new() {
                        Ok(out) => out,
                        Err(e) => {
                            error!(
                                "AudioThread (lazy): failed to initialise audio output: {}",
                                e
                            );
                            // Drain the channel so senders don't block/leak.
                            drop(rx);
                            return;
                        }
                    };

                    info!("AudioThread (lazy) started");
                    run_audio_thread(
                        rx,
                        startup_backlog,
                        output,
                        host_tx,
                        wakeup_for_thread,
                        http_client_factory,
                    );
                    info!("AudioThread (lazy) stopped");
                })); // end catch_unwind
                if let Err(panic_info) = result {
                    let msg = if let Some(s) = panic_info.downcast_ref::<String>() {
                        s.clone()
                    } else if let Some(s) = panic_info.downcast_ref::<&str>() {
                        s.to_string()
                    } else {
                        "Unknown panic".to_string()
                    };
                    error!("[AudioThread lazy] PANIC: {}", msg);
                }
            })
            .map_err(|e| {
                EngineError::from_detail(
                    ErrorCode::IoError,
                    format!("Failed to spawn audio thread: {}", e),
                )
            })?;

        let thread_id = handle.thread().id();

        Ok(Self {
            tx,
            wakeup,
            handle: Some(handle),
            thread_id,
        })
    }

    pub fn shutdown(&mut self) {
        let _ = self.tx.try_send(AudioCmd::Shutdown);
        self.wakeup.notify();
        if let Some(h) = self.handle.take() {
            join_with_timeout(h, Duration::from_secs(3), "audio-thread");
        }
    }
}

impl Drop for AudioThread {
    fn drop(&mut self) {
        let _ = self.tx.try_send(AudioCmd::Shutdown);
        self.wakeup.notify();

        // Never join from inside the audio thread itself
        if thread::current().id() == self.thread_id {
            self.handle.take();
            return;
        }

        if let Some(h) = self.handle.take() {
            join_with_timeout(h, Duration::from_secs(3), "audio-thread");
        }
    }
}

/// Frames the graph renders per `process()` call.
///
/// Web Audio's render quantum, and the grain at which `start(when)`,
/// `stop(when)` and k-rate automation take effect. Kept separate from the ring
/// block size below, which is about how often the ring is pushed to, not about
/// scheduling accuracy.
pub(crate) const RENDER_QUANTUM_FRAMES: usize = 128;

/// Calculate optimal process buffer size based on sample rate.
/// Higher sample rates need larger buffers for the same latency.
#[inline]
fn calculate_process_frames(sample_rate: u32) -> usize {
    // Target ~21ms of audio data
    // 48kHz * 0.021 ≈ 1008 frames, round to 1024 for alignment
    // 44.1kHz * 0.021 ≈ 926 frames, round to 1024
    // Higher rates like 96kHz would use 2048
    let target_ms = 21.0;
    let frames = (sample_rate as f32 * target_ms / 1000.0) as usize;
    // Round up to nearest power of 2 for better cache alignment
    frames.next_power_of_two().max(512).min(4096)
}

/// Step 3 of one audio-thread tick: service every player once — drain what the
/// network delivered, adopt a finished stream into the cache, and hand out the
/// events the player raised.
///
/// A free function over an event sink rather than a method on the loop's locals,
/// because this is the tick's steady per-player work and Section 7.3 requires it
/// to be measured. `HostTx` and `AudioOutput` are the two things a host test
/// binary cannot build; the sink removes the first and this step never touches
/// the second. The production call site passes a closure over `host_tx`, which
/// captures by reference and allocates nothing.
fn service_players(
    inner_players: &mut HashMap<InnerAudioId, InnerAudioPlayer>,
    audio_cache: &GlobalAudioCache,
    mut emit: impl FnMut(InnerAudioEvent),
) {
    for player in inner_players.values_mut() {
        player.poll_stream();

        // Cache completed streaming audio
        if player.is_stream_complete() {
            if let Some(url) = player.loading_url().map(|s| s.to_string()) {
                if let Some(audio) = player.take_streamed_audio() {
                    let cached = audio_cache.insert(url, audio);
                    // Ownership-only handoff: the stream may already be
                    // playing at a non-zero position.
                    player.attach_cached_backing(cached);
                }
            }
        }

        // Hand the player's events to the caller
        for event in player.drain_events() {
            emit(event);
        }
    }
}

fn wait_for_audio_work(wakeup: &ThreadWakeup, mode: AudioWaitMode) {
    match mode {
        AudioWaitMode::Continue => {}
        AudioWaitMode::Indefinite => wakeup.wait(),
        AudioWaitMode::Timed(duration) => {
            wakeup.wait_timeout(duration);
        }
    }
}

/// The next command to execute: the startup backlog before the channel.
///
/// **The order is the point, not a preference.** `AudioCmd` carries ids allocated
/// on the JavaScript side and its creates are fire-and-forget, so a command that
/// arrives out of order addresses a node that does not exist yet. The backlog
/// holds commands the service accepted *before* this thread existed, which are by
/// construction older than anything still sitting in the channel — so consulting
/// the channel first would invert the only ordering this protocol has.
///
/// Its own function so that ordering is observable without an audio device, which
/// `run_audio_thread` needs and a host test cannot provide.
fn next_command(
    startup_backlog: &mut std::vec::IntoIter<AudioCmd>,
    rx: &AudioCommandReceiver,
) -> Option<AudioCmd> {
    if startup_backlog.len() != 0 {
        if let Ok(urgent) = rx.try_recv_urgent() {
            return Some(urgent);
        }
        return startup_backlog.next();
    }
    rx.try_recv().ok()
}

/// Amplitude below which the limiter is transparent. Above it, the curve bends.
const SOFT_LIMIT_KNEE: f32 = 0.75;

/// Apply one soft limit over a finished mix, in place.
///
/// **One pass over the whole mix, not one per contributor.** This used to live
/// inside `AudioContext::process`, which is additive: it ran once per context,
/// in `HashMap` iteration order, over a partial sum containing other contexts'
/// audio -- so the same input produced different output run to run -- and the
/// InnerAudio players, which mix in after every context, were never limited at
/// all.
///
/// The curve is continuous and smooth at the knee. The previous one was neither:
/// it left `1.0` untouched and mapped `1.0 + eps` to `0.5`, a 6 dB step at the
/// threshold, so the "soft" clip introduced a harder edge than the hard clamp it
/// replaced. Here `u` is how far into the knee the sample is, `u / (1 + u)`
/// starts at zero with unit slope and asymptotes to one, so the result matches
/// the input's value and slope at the knee and never leaves [-1, 1].
fn soft_limit(buffer: &mut [f32]) {
    // Most blocks never exceed the knee; one scan is cheaper than a branch and a
    // division per sample.
    if !buffer.iter().any(|s| s.abs() > SOFT_LIMIT_KNEE) {
        return;
    }

    let headroom = 1.0 - SOFT_LIMIT_KNEE;
    for sample in buffer.iter_mut() {
        let magnitude = sample.abs();
        if magnitude > SOFT_LIMIT_KNEE {
            let u = (magnitude - SOFT_LIMIT_KNEE) / headroom;
            *sample = sample.signum() * (SOFT_LIMIT_KNEE + headroom * u / (1.0 + u));
        }
    }
}

/// Audio thread main loop — 3-level power management.
///
/// # Power States
///
/// | State       | Tick      | When                                          |
/// |-------------|-----------|-----------------------------------------------|
/// | **Active**  |   5 ms    | context Running / player Playing / streaming  |
/// | **LowPower**|  50 ms    | idle < 3 s (recently stopped)                 |
/// | **Sleep**   | event     | idle >= 3 s (stream paused, condvar wait)     |
///
/// In all states, the thread sleeps on a [`ThreadWakeup`] condvar so that
/// incoming commands (via [`AudioSender`](shared::op_state::AudioSender))
/// wake it without waiting for the next timed tick.
fn run_audio_thread(
    rx: AudioCommandReceiver,
    startup_backlog: Vec<AudioCmd>,
    mut output: AudioOutput,
    host_tx: HostTx,
    wakeup: ThreadWakeup,
    http_client_factory: StreamingHttpClientFactory,
) {
    // Consumed ahead of the channel in the drain below, so the commands the
    // service buffered before this thread existed keep their place in the order.
    let mut startup_backlog = startup_backlog.into_iter();
    let mut sample_rate = output.sample_rate();
    let mut channels = output.channels();

    // Pre-allocate with reasonable capacity to avoid rehashing
    let mut contexts: HashMap<AudioContextId, AudioContext> = HashMap::with_capacity(4);

    // Node-to-context index for O(1) lookup
    let mut node_index = NodeContextIndex::new();

    // InnerAudioContext players with pre-allocated capacity
    let mut inner_players: HashMap<InnerAudioId, InnerAudioPlayer> = HashMap::with_capacity(16);

    // MediaAudioPlayer: maps player_id -> list of InnerAudioContext source IDs
    let mut media_players: HashMap<u32, Vec<InnerAudioId>> = HashMap::with_capacity(4);

    // Global audio cache (64MB default)
    let audio_cache = GlobalAudioCache::new();

    // Per-host and lazy: local audio never builds an HTTP pool, while every
    // remote cache miss after the first reuses the same DNS/TCP/TLS/H2 state.
    let mut streaming_client = LazyStreamingClient::new(http_client_factory);

    // Channel for receiving decode+resample results from worker threads.
    let (decode_tx, decode_rx) =
        decode_queue::<DecodeResult>(DECODE_RESULT_QUEUE_CAPACITY, MAX_DECODE_RESULT_QUEUED_BYTES);

    // The fixed-size pool starts only if a decode job arrives. Once started,
    // its two workers persist for the audio thread's lifetime.
    let mut decode_pool = LazyDecodePool::new(decode_tx, sample_rate, wakeup.clone());

    // Audio processing buffer - dynamically sized based on sample rate
    let mut process_frames = calculate_process_frames(sample_rate);
    let mut buffer_size = process_frames * channels as usize;
    let mut process_buffer = vec![0.0f32; buffer_size];

    // Get sync handle for callback-driven wakeup
    let mut sync = output.sync().clone();

    // Pause state: when true, skip audio processing but still handle commands.
    let mut paused = false;

    // Management work (including streaming) and audible output have separate
    // idle clocks so a download does not keep the hardware callback running.
    let power_config = AudioPowerConfig::default();
    let stream_retry_delay = power_config.sleep_tick;
    let mut power = AudioPowerManager::new(power_config.clone());
    let mut output_power = AudioPowerManager::new(power_config);
    let mut stream_gate = AudioStreamGate::new_running();

    // Exponential backoff for audio output recovery
    let mut recovery_delay = Duration::from_secs(1);
    const MAX_RECOVERY_DELAY: Duration = Duration::from_secs(30);

    loop {
        // -----------------------------------------------------------------
        // 1. Drain pending commands (non-blocking, rate-limited).
        //    Cap at 256 commands per iteration to prevent mixing starvation
        //    when JS fires rapid bursts (automation, game SFX).  Remaining
        //    commands are picked up on the next iteration (~5ms in Active).
        // -----------------------------------------------------------------
        let mut cmd_count = 0usize;
        while cmd_count < AUDIO_COMMANDS_PER_DRAIN {
            let cmd = match next_command(&mut startup_backlog, &rx) {
                Some(c) => c,
                None => break,
            };
            cmd_count += 1;
            match cmd {
                AudioCmd::Shutdown => {
                    return;
                }

                AudioCmd::PauseAll => {
                    if !paused {
                        paused = true;
                        for ctx in contexts.values_mut() {
                            ctx.pause_clock_for_background();
                        }
                        info!("AudioThread pause requested");
                    }
                }

                AudioCmd::ResumeAll => {
                    if paused {
                        paused = false;
                        for ctx in contexts.values_mut() {
                            ctx.resume_clock_after_background();
                        }
                        info!("AudioThread resume requested");
                    }
                }

                AudioCmd::CreateContext {
                    ctx_id,
                    sample_rate: req_rate,
                } => {
                    let rate = req_rate.unwrap_or(sample_rate);
                    if !create_context_scoped(&mut contexts, ctx_id, rate, channels) {
                        warn!("CreateContext ignored duplicate AudioContext id {ctx_id}");
                    }
                }

                AudioCmd::ReleaseContext { ctx_id } => {
                    release_context_scoped(&mut contexts, &mut node_index, ctx_id);
                }

                AudioCmd::ReleaseAllContexts => {
                    handle_release_all_contexts(
                        &mut startup_backlog,
                        &rx,
                        &mut contexts,
                        &mut node_index,
                    );
                }

                AudioCmd::CloseContext { ctx_id, resp } => {
                    if release_context_scoped(&mut contexts, &mut node_index, ctx_id) {
                        let _ = resp.send(Ok(()));
                    } else {
                        let _ = resp.send(Err(EngineError::from_detail(
                            ErrorCode::NotFound,
                            format!("AudioContext {} not found", ctx_id),
                        )));
                    }
                }

                AudioCmd::GetContextState { ctx_id, resp } => {
                    if let Some(ctx) = contexts.get(&ctx_id) {
                        let _ = resp.send(Ok(ctx.state()));
                    } else {
                        let _ = resp.send(Err(EngineError::from_detail(
                            ErrorCode::NotFound,
                            format!("AudioContext {} not found", ctx_id),
                        )));
                    }
                }

                AudioCmd::ResumeContext { ctx_id, resp } => {
                    if let Some(ctx) = contexts.get_mut(&ctx_id) {
                        ctx.resume();
                        let _ = resp.send(Ok(()));
                    } else {
                        let _ = resp.send(Err(EngineError::from_detail(
                            ErrorCode::NotFound,
                            format!("AudioContext {} not found", ctx_id),
                        )));
                    }
                }

                AudioCmd::SuspendContext { ctx_id, resp } => {
                    if let Some(ctx) = contexts.get_mut(&ctx_id) {
                        ctx.suspend();
                        let _ = resp.send(Ok(()));
                    } else {
                        let _ = resp.send(Err(EngineError::from_detail(
                            ErrorCode::NotFound,
                            format!("AudioContext {} not found", ctx_id),
                        )));
                    }
                }

                AudioCmd::DecodeAudioData { ctx_id, data, resp } => {
                    if contexts.contains_key(&ctx_id) {
                        decode_pool.submit(DecodeJob::AudioBuffer { ctx_id, data, resp });
                    } else {
                        let _ = resp.send(Err(EngineError::from_detail(
                            ErrorCode::NotFound,
                            format!("AudioContext {} not found", ctx_id),
                        )));
                    }
                }

                AudioCmd::ReleaseBuffer { ctx_id, buffer_id } => {
                    release_buffer_scoped(&mut contexts, ctx_id, buffer_id);
                }

                AudioCmd::ReleaseNode { ctx_id, node_id } => {
                    release_node_scoped(&mut contexts, &mut node_index, ctx_id, node_id);
                }

                AudioCmd::CreateBufferSource { ctx_id, node_id } => {
                    if let Some(ctx) = contexts.get_mut(&ctx_id) {
                        ctx.create_buffer_source(node_id);
                        // Register node in index for fast lookup
                        node_index.register(node_id, ctx_id);
                    } else {
                        tracing::warn!("CreateBufferSource: AudioContext {} not found", ctx_id);
                    }
                }

                AudioCmd::SetBuffer {
                    ctx_id,
                    node_id,
                    buffer_id,
                } => {
                    if !set_buffer_scoped(&mut contexts, &node_index, ctx_id, node_id, buffer_id) {
                        warn!(
                            "SetBuffer rejected: context {ctx_id} does not own node {node_id} or buffer {buffer_id:?}"
                        );
                    }
                }

                AudioCmd::SetStartedBuffer {
                    ctx_id,
                    node_id,
                    buffer,
                } => {
                    if !set_started_buffer_scoped(
                        &mut contexts,
                        &node_index,
                        ctx_id,
                        node_id,
                        buffer,
                    ) {
                        warn!(
                            "SetStartedBuffer rejected: context {ctx_id} does not own node {node_id}"
                        );
                    }
                }

                AudioCmd::StartBuffer {
                    ctx_id,
                    node_id,
                    buffer,
                    when,
                    offset,
                    duration,
                } => {
                    if !start_buffer_scoped(
                        &mut contexts,
                        &node_index,
                        ctx_id,
                        node_id,
                        buffer,
                        when,
                        offset,
                        duration,
                    ) {
                        warn!("StartBuffer rejected: context {ctx_id} does not own node {node_id}");
                    }
                }

                AudioCmd::Start {
                    node_id,
                    when,
                    offset,
                    duration,
                    resp,
                } => {
                    // Use index for O(1) context lookup
                    let found = node_index
                        .get_context(node_id)
                        .and_then(|ctx_id| contexts.get_mut(&ctx_id))
                        .map(|ctx| ctx.start_source(node_id, when, offset, duration))
                        .unwrap_or(false);

                    if found {
                        let _ = resp.send(Ok(()));
                    } else {
                        let _ = resp.send(Err(EngineError::from_detail(
                            ErrorCode::NotFound,
                            format!("AudioBufferSourceNode {} not found", node_id),
                        )));
                    }
                }

                AudioCmd::Stop {
                    node_id,
                    when,
                    resp,
                } => {
                    // Stop the node, then check whether it finished immediately
                    // (stop(when<=0)). If so, fully remove it now (index + context
                    // maps) so a stop on a *suspended* context doesn't linger until
                    // resume/close. A future-dated stop stays reachable and is swept
                    // by context.process() when it actually finishes.
                    let found = match node_index
                        .get_context(node_id)
                        .and_then(|ctx_id| contexts.get_mut(&ctx_id))
                    {
                        Some(ctx) => {
                            let found = ctx.stop_source(node_id, when);
                            if found {
                                for &removed in ctx.remove_finished_node(node_id) {
                                    node_index.unregister(removed);
                                }
                            }
                            found
                        }
                        None => false,
                    };

                    if found {
                        let _ = resp.send(Ok(()));
                    } else {
                        let _ = resp.send(Err(EngineError::from_detail(
                            ErrorCode::NotFound,
                            format!("AudioBufferSourceNode {} not found", node_id),
                        )));
                    }
                }

                AudioCmd::SetLoop {
                    node_id,
                    loop_enabled,
                    loop_start,
                    loop_end,
                } => {
                    tracing::trace!(
                        "SetLoop: node_id={}, enabled={}, start={}, end={}",
                        node_id,
                        loop_enabled,
                        loop_start,
                        loop_end
                    );
                    // Use index for O(1) context lookup
                    let found = node_index
                        .get_context(node_id)
                        .and_then(|ctx_id| contexts.get_mut(&ctx_id))
                        .map(|ctx| ctx.set_loop(node_id, loop_enabled, loop_start, loop_end))
                        .unwrap_or(false);

                    if !found {
                        tracing::warn!("SetLoop: node {} not found", node_id);
                    }
                }

                AudioCmd::SetPlaybackRate {
                    node_id,
                    rate,
                    resp,
                } => {
                    // Use index for O(1) context lookup
                    let found = node_index
                        .get_context(node_id)
                        .and_then(|ctx_id| contexts.get_mut(&ctx_id))
                        .map(|ctx| ctx.set_playback_rate(node_id, rate))
                        .unwrap_or(false);

                    if found {
                        let _ = resp.send(Ok(()));
                    } else {
                        let _ = resp.send(Err(EngineError::from_detail(
                            ErrorCode::NotFound,
                            format!("AudioBufferSourceNode {} not found", node_id),
                        )));
                    }
                }

                AudioCmd::CreateGain { ctx_id, node_id } => {
                    if let Some(ctx) = contexts.get_mut(&ctx_id) {
                        ctx.create_gain(node_id);
                        // Register node in index for fast lookup
                        node_index.register(node_id, ctx_id);
                    } else {
                        tracing::warn!("CreateGain: AudioContext {} not found", ctx_id);
                    }
                }

                AudioCmd::SetGainValue { node_id, value } => {
                    // Use index for O(1) context lookup
                    let found = node_index
                        .get_context(node_id)
                        .and_then(|ctx_id| contexts.get_mut(&ctx_id))
                        .map(|ctx| ctx.set_gain(node_id, value))
                        .unwrap_or(false);

                    if !found {
                        tracing::warn!("SetGainValue: GainNode {} not found", node_id);
                    }
                }

                AudioCmd::SetNodeParam {
                    node_id,
                    param_name,
                    value,
                } => {
                    let found = node_index
                        .get_context(node_id)
                        .and_then(|ctx_id| contexts.get_mut(&ctx_id))
                        .map(|ctx| ctx.set_node_param(node_id, &param_name, value))
                        .unwrap_or(false);

                    if !found {
                        tracing::warn!(
                            "SetNodeParam: node {} param {} not found",
                            node_id,
                            param_name
                        );
                    }
                }

                // ==================== Phase 2 Nodes ====================
                AudioCmd::CreateOscillator { ctx_id, node_id } => {
                    if let Some(ctx) = contexts.get_mut(&ctx_id) {
                        ctx.add_node(Box::new(OscillatorNode::new(node_id, sample_rate)));
                        node_index.register(node_id, ctx_id);
                    }
                }

                AudioCmd::SetOscillatorType { node_id, osc_type } => {
                    if let Some(ctx_id) = node_index.get_context(node_id) {
                        if let Some(ctx) = contexts.get_mut(&ctx_id) {
                            ctx.with_node_typed::<OscillatorNode, _>(node_id, |osc| {
                                osc.set_type(OscillatorType::from_str(&osc_type));
                            });
                        }
                    }
                }

                AudioCmd::StartOscillator { node_id, when } => {
                    if let Some(ctx_id) = node_index.get_context(node_id) {
                        if let Some(ctx) = contexts.get_mut(&ctx_id) {
                            ctx.with_node_typed::<OscillatorNode, _>(node_id, |osc| {
                                osc.start(when);
                            });
                        }
                    }
                }

                AudioCmd::StopOscillator { node_id, when } => {
                    if let Some(ctx_id) = node_index.get_context(node_id) {
                        if let Some(ctx) = contexts.get_mut(&ctx_id) {
                            ctx.with_node_typed::<OscillatorNode, _>(node_id, |osc| {
                                osc.stop(when);
                            });
                        }
                    }
                    // Unregistered by the finished-node sweep in context.process()
                    // once the scheduled stop time is actually reached.
                }

                AudioCmd::CreateDelay {
                    ctx_id,
                    node_id,
                    max_delay_time,
                } => {
                    if let Some(ctx) = contexts.get_mut(&ctx_id) {
                        let sr = ctx.sample_rate();
                        let ch = ctx.channels();
                        ctx.add_node(Box::new(DelayNode::new(node_id, max_delay_time, sr, ch)));
                        node_index.register(node_id, ctx_id);
                    }
                }

                AudioCmd::CreateBiquadFilter { ctx_id, node_id } => {
                    if let Some(ctx) = contexts.get_mut(&ctx_id) {
                        let ch = ctx.channels();
                        ctx.add_node(Box::new(BiquadFilterNode::new(node_id, ch, sample_rate)));
                        node_index.register(node_id, ctx_id);
                    }
                }

                AudioCmd::SetBiquadFilterType {
                    node_id,
                    filter_type,
                } => {
                    if let Some(ctx_id) = node_index.get_context(node_id) {
                        if let Some(ctx) = contexts.get_mut(&ctx_id) {
                            ctx.with_node_typed::<BiquadFilterNode, _>(node_id, |filt| {
                                filt.set_type(BiquadFilterType::from_str(&filter_type));
                            });
                        }
                    }
                }

                AudioCmd::CreateWaveShaper { ctx_id, node_id } => {
                    if let Some(ctx) = contexts.get_mut(&ctx_id) {
                        ctx.add_node(Box::new(WaveShaperNode::new(node_id)));
                        node_index.register(node_id, ctx_id);
                    }
                }

                AudioCmd::SetWaveShaperCurve { node_id, curve } => {
                    if let Some(ctx_id) = node_index.get_context(node_id) {
                        if let Some(ctx) = contexts.get_mut(&ctx_id) {
                            ctx.with_node_typed::<WaveShaperNode, _>(node_id, |ws| {
                                ws.set_curve(curve);
                            });
                        }
                    }
                }

                AudioCmd::SetWaveShaperOversample {
                    node_id,
                    oversample,
                } => {
                    if let Some(ctx_id) = node_index.get_context(node_id) {
                        if let Some(ctx) = contexts.get_mut(&ctx_id) {
                            ctx.with_node_typed::<WaveShaperNode, _>(node_id, |ws| {
                                ws.set_oversample(OversampleType::from_str(&oversample));
                            });
                        }
                    }
                }

                AudioCmd::CreateAnalyser { ctx_id, node_id } => {
                    if let Some(ctx) = contexts.get_mut(&ctx_id) {
                        let ch = ctx.channels();
                        ctx.add_node(Box::new(AnalyserNode::new(node_id, ch)));
                        node_index.register(node_id, ctx_id);
                    }
                }

                AudioCmd::SetAnalyserFftSize { node_id, fft_size } => {
                    if let Some(ctx_id) = node_index.get_context(node_id) {
                        if let Some(ctx) = contexts.get_mut(&ctx_id) {
                            ctx.with_node_typed::<AnalyserNode, _>(node_id, |an| {
                                an.set_fft_size(fft_size as usize);
                            });
                        }
                    }
                }

                AudioCmd::GetAnalyserByteTimeDomainData { node_id, resp } => {
                    let result = node_index
                        .get_context(node_id)
                        .and_then(|ctx_id| contexts.get_mut(&ctx_id))
                        .and_then(|ctx| {
                            ctx.with_node_typed::<AnalyserNode, _>(node_id, |an| {
                                an.get_byte_time_domain_data()
                            })
                        });
                    match result {
                        Some(data) => {
                            let _ = resp.send(Ok(data));
                        }
                        None => {
                            let _ = resp.send(Err(EngineError::from_detail(
                                ErrorCode::NotFound,
                                format!("AnalyserNode {} not found", node_id),
                            )));
                        }
                    }
                }

                AudioCmd::GetAnalyserFloatTimeDomainData { node_id, resp } => {
                    let result = node_index
                        .get_context(node_id)
                        .and_then(|ctx_id| contexts.get_mut(&ctx_id))
                        .and_then(|ctx| {
                            ctx.with_node_typed::<AnalyserNode, _>(node_id, |an| {
                                an.get_float_time_domain_data()
                            })
                        });
                    match result {
                        Some(data) => {
                            let _ = resp.send(Ok(data));
                        }
                        None => {
                            let _ = resp.send(Err(EngineError::from_detail(
                                ErrorCode::NotFound,
                                format!("AnalyserNode {} not found", node_id),
                            )));
                        }
                    }
                }

                // ==================== Phase 3 Nodes ====================
                AudioCmd::CreateDynamicsCompressor { ctx_id, node_id } => {
                    if let Some(ctx) = contexts.get_mut(&ctx_id) {
                        let ch = ctx.channels();
                        ctx.add_node(Box::new(DynamicsCompressorNode::new(node_id, ch)));
                        node_index.register(node_id, ctx_id);
                    }
                }

                AudioCmd::CreatePanner { ctx_id, node_id } => {
                    if let Some(ctx) = contexts.get_mut(&ctx_id) {
                        ctx.add_node(Box::new(PannerNode::new(node_id)));
                        node_index.register(node_id, ctx_id);
                    }
                }

                AudioCmd::SetPanningModel { node_id, model } => {
                    if let Some(ctx_id) = node_index.get_context(node_id) {
                        if let Some(ctx) = contexts.get_mut(&ctx_id) {
                            ctx.with_node_typed::<PannerNode, _>(node_id, |p| {
                                p.set_panning_model(PanningModel::from_str(&model));
                            });
                        }
                    }
                }

                AudioCmd::SetDistanceModel { node_id, model } => {
                    if let Some(ctx_id) = node_index.get_context(node_id) {
                        if let Some(ctx) = contexts.get_mut(&ctx_id) {
                            ctx.with_node_typed::<PannerNode, _>(node_id, |p| {
                                p.set_distance_model(DistanceModel::from_str(&model));
                            });
                        }
                    }
                }

                AudioCmd::SetAnalyserScalar {
                    node_id,
                    prop,
                    value,
                } => {
                    if let Some(ctx_id) = node_index.get_context(node_id) {
                        if let Some(ctx) = contexts.get_mut(&ctx_id) {
                            ctx.with_node_typed::<AnalyserNode, _>(node_id, |an| {
                                match prop.as_str() {
                                    "minDecibels" => an.set_min_decibels(value),
                                    "maxDecibels" => an.set_max_decibels(value),
                                    "smoothingTimeConstant" => {
                                        an.set_smoothing_time_constant(value)
                                    }
                                    _ => {}
                                }
                            });
                        }
                    }
                }

                AudioCmd::SetPannerScalar {
                    node_id,
                    prop,
                    value,
                } => {
                    if let Some(ctx_id) = node_index.get_context(node_id) {
                        if let Some(ctx) = contexts.get_mut(&ctx_id) {
                            ctx.with_node_typed::<PannerNode, _>(node_id, |p| {
                                match prop.as_str() {
                                    "refDistance" => p.set_ref_distance(value),
                                    "maxDistance" => p.set_max_distance(value),
                                    "rolloffFactor" => p.set_rolloff_factor(value),
                                    "coneInnerAngle" => p.set_cone_inner_angle(value),
                                    "coneOuterAngle" => p.set_cone_outer_angle(value),
                                    "coneOuterGain" => p.set_cone_outer_gain(value),
                                    _ => {}
                                }
                            });
                        }
                    }
                }

                AudioCmd::CreateChannelMerger {
                    ctx_id,
                    node_id,
                    number_of_inputs,
                } => {
                    if let Some(ctx) = contexts.get_mut(&ctx_id) {
                        ctx.add_node(Box::new(ChannelMergerNode::new(node_id, number_of_inputs)));
                        node_index.register(node_id, ctx_id);
                    }
                }

                AudioCmd::CreateChannelSplitter {
                    ctx_id,
                    node_id,
                    number_of_outputs,
                } => {
                    if let Some(ctx) = contexts.get_mut(&ctx_id) {
                        ctx.add_node(Box::new(ChannelSplitterNode::new(
                            node_id,
                            number_of_outputs,
                        )));
                        node_index.register(node_id, ctx_id);
                    }
                }

                AudioCmd::CreateConstantSource { ctx_id, node_id } => {
                    if let Some(ctx) = contexts.get_mut(&ctx_id) {
                        ctx.add_node(Box::new(ConstantSourceNode::new(node_id)));
                        node_index.register(node_id, ctx_id);
                    }
                }

                AudioCmd::StartConstantSource { node_id, when } => {
                    if let Some(ctx_id) = node_index.get_context(node_id) {
                        if let Some(ctx) = contexts.get_mut(&ctx_id) {
                            ctx.with_node_typed::<ConstantSourceNode, _>(node_id, |cs| {
                                cs.start(when);
                            });
                        }
                    }
                }

                AudioCmd::StopConstantSource { node_id, when } => {
                    if let Some(ctx_id) = node_index.get_context(node_id) {
                        if let Some(ctx) = contexts.get_mut(&ctx_id) {
                            ctx.with_node_typed::<ConstantSourceNode, _>(node_id, |cs| {
                                cs.stop(when);
                            });
                        }
                    }
                    // Unregistered by the finished-node sweep in context.process()
                    // once the scheduled stop time is actually reached.
                }

                AudioCmd::CreateIIRFilter {
                    ctx_id,
                    node_id,
                    feedforward,
                    feedback,
                } => {
                    if let Some(ctx) = contexts.get_mut(&ctx_id) {
                        let ch = ctx.channels();
                        ctx.add_node(Box::new(IIRFilterNode::new(
                            node_id,
                            feedforward,
                            feedback,
                            ch,
                        )));
                        node_index.register(node_id, ctx_id);
                    }
                }

                AudioCmd::Connect {
                    src,
                    src_output,
                    dst,
                    dst_input,
                    resp,
                } => {
                    // Use index to find the context containing the source node
                    let found = node_index
                        .get_context(src)
                        .and_then(|ctx_id| contexts.get_mut(&ctx_id))
                        .map(|ctx| {
                            ctx.connect_ports(src, src_output, dst, dst_input);
                            true
                        })
                        .unwrap_or(false);

                    if found {
                        let _ = resp.send(Ok(()));
                    } else {
                        let _ = resp.send(Err(EngineError::from_detail(
                            ErrorCode::NotFound,
                            "No context found",
                        )));
                    }
                }

                AudioCmd::Disconnect { node_id, dst, resp } => {
                    // Use index for O(1) context lookup
                    if let Some(ctx_id) = node_index.get_context(node_id) {
                        if let Some(ctx) = contexts.get_mut(&ctx_id) {
                            ctx.disconnect(node_id, dst);
                        }
                    }
                    let _ = resp.send(Ok(()));
                }

                // ==================== Frequency Response & Analysis ====================
                AudioCmd::GetFrequencyResponse {
                    node_id,
                    frequencies,
                    resp,
                } => {
                    let result = node_index
                        .get_context(node_id)
                        .and_then(|ctx_id| contexts.get_mut(&ctx_id))
                        .and_then(|ctx| {
                            let sr = ctx.sample_rate() as f64;
                            // Try BiquadFilterNode first
                            if let Some(r) = ctx
                                .with_node_typed::<BiquadFilterNode, _>(node_id, |n| {
                                    n.get_frequency_response(sr, &frequencies)
                                })
                            {
                                return Some(r);
                            }
                            // Try IIRFilterNode
                            ctx.with_node_typed::<IIRFilterNode, _>(node_id, |n| {
                                n.get_frequency_response(sr, &frequencies)
                            })
                        });
                    match result {
                        Some(data) => {
                            let _ = resp.send(Ok(data));
                        }
                        None => {
                            let _ = resp.send(Err(EngineError::from_detail(
                                ErrorCode::NotFound,
                                format!("Filter node {} not found", node_id),
                            )));
                        }
                    }
                }

                AudioCmd::GetReduction { node_id, resp } => {
                    let result = node_index
                        .get_context(node_id)
                        .and_then(|ctx_id| contexts.get_mut(&ctx_id))
                        .and_then(|ctx| {
                            ctx.with_node_typed::<DynamicsCompressorNode, _>(node_id, |n| {
                                n.reduction()
                            })
                        });
                    match result {
                        Some(val) => {
                            let _ = resp.send(Ok(val));
                        }
                        None => {
                            let _ = resp.send(Err(EngineError::from_detail(
                                ErrorCode::NotFound,
                                format!("DynamicsCompressorNode {} not found", node_id),
                            )));
                        }
                    }
                }

                AudioCmd::GetAnalyserByteFrequencyData { node_id, resp } => {
                    let result = node_index
                        .get_context(node_id)
                        .and_then(|ctx_id| contexts.get_mut(&ctx_id))
                        .and_then(|ctx| {
                            ctx.with_node_typed::<AnalyserNode, _>(node_id, |an| {
                                an.get_byte_frequency_data()
                            })
                        });
                    match result {
                        Some(data) => {
                            let _ = resp.send(Ok(data));
                        }
                        None => {
                            let _ = resp.send(Err(EngineError::from_detail(
                                ErrorCode::NotFound,
                                format!("AnalyserNode {} not found", node_id),
                            )));
                        }
                    }
                }

                AudioCmd::GetAnalyserFloatFrequencyData { node_id, resp } => {
                    let result = node_index
                        .get_context(node_id)
                        .and_then(|ctx_id| contexts.get_mut(&ctx_id))
                        .and_then(|ctx| {
                            ctx.with_node_typed::<AnalyserNode, _>(node_id, |an| {
                                an.get_float_frequency_data()
                            })
                        });
                    match result {
                        Some(data) => {
                            let _ = resp.send(Ok(data));
                        }
                        None => {
                            let _ = resp.send(Err(EngineError::from_detail(
                                ErrorCode::NotFound,
                                format!("AnalyserNode {} not found", node_id),
                            )));
                        }
                    }
                }

                // ==================== AudioParam Automation ====================
                AudioCmd::AudioParamSetValueAtTime {
                    node_id,
                    param_name,
                    value,
                    time,
                } => {
                    if let Some(ctx_id) = node_index.get_context(node_id) {
                        if let Some(ctx) = contexts.get_mut(&ctx_id) {
                            ctx.param_set_value_at_time(node_id, &param_name, value, time);
                        }
                    }
                }

                AudioCmd::AudioParamLinearRamp {
                    node_id,
                    param_name,
                    value,
                    end_time,
                } => {
                    if let Some(ctx_id) = node_index.get_context(node_id) {
                        if let Some(ctx) = contexts.get_mut(&ctx_id) {
                            ctx.param_linear_ramp(node_id, &param_name, value, end_time);
                        }
                    }
                }

                AudioCmd::AudioParamExponentialRamp {
                    node_id,
                    param_name,
                    value,
                    end_time,
                } => {
                    if let Some(ctx_id) = node_index.get_context(node_id) {
                        if let Some(ctx) = contexts.get_mut(&ctx_id) {
                            ctx.param_exponential_ramp(node_id, &param_name, value, end_time);
                        }
                    }
                }

                AudioCmd::AudioParamSetTarget {
                    node_id,
                    param_name,
                    target,
                    start_time,
                    time_constant,
                } => {
                    if let Some(ctx_id) = node_index.get_context(node_id) {
                        if let Some(ctx) = contexts.get_mut(&ctx_id) {
                            ctx.param_set_target(
                                node_id,
                                &param_name,
                                target,
                                start_time,
                                time_constant,
                            );
                        }
                    }
                }

                AudioCmd::AudioParamCancelScheduled {
                    node_id,
                    param_name,
                    cancel_time,
                } => {
                    if let Some(ctx_id) = node_index.get_context(node_id) {
                        if let Some(ctx) = contexts.get_mut(&ctx_id) {
                            ctx.param_cancel_scheduled(node_id, &param_name, cancel_time);
                        }
                    }
                }

                // ==================== Buffer Data Access ====================
                AudioCmd::CreateBuffer {
                    ctx_id,
                    channels,
                    length,
                    sample_rate: buf_rate,
                    resp,
                } => {
                    if let Some(ctx) = contexts.get_mut(&ctx_id) {
                        match ctx.create_empty_buffer(channels, length, buf_rate) {
                            Ok(id) => {
                                let _ = resp.send(Ok(AudioBufferInfo {
                                    id,
                                    duration: length as f64 / buf_rate as f64,
                                    sample_rate: buf_rate,
                                    channels,
                                    length,
                                }));
                            }
                            Err(e) => {
                                let _ = resp.send(Err(e));
                            }
                        }
                    } else {
                        let _ = resp.send(Err(EngineError::from_detail(
                            ErrorCode::NotFound,
                            format!("AudioContext {} not found", ctx_id),
                        )));
                    }
                }

                AudioCmd::GetChannelData {
                    ctx_id,
                    buffer_id,
                    channel,
                    resp,
                } => {
                    if let Some(ctx) = contexts.get(&ctx_id) {
                        if let Some(data) = ctx.get_channel_data(buffer_id, channel) {
                            let _ = resp.send(Ok(data));
                        } else {
                            let _ = resp.send(Err(EngineError::from_detail(
                                ErrorCode::NotFound,
                                format!("Buffer {} channel {} not found", buffer_id, channel),
                            )));
                        }
                    } else {
                        let _ = resp.send(Err(EngineError::from_detail(
                            ErrorCode::NotFound,
                            format!("AudioContext {} not found", ctx_id),
                        )));
                    }
                }

                AudioCmd::TakeDecodedBufferData {
                    ctx_id,
                    buffer_id,
                    resp,
                } => {
                    if let Some(ctx) = contexts.get_mut(&ctx_id) {
                        match ctx.take_decoded_buffer_data(buffer_id) {
                            Ok(Some(data)) => {
                                let _ = resp.send(Ok(data));
                            }
                            Ok(None) => {
                                let _ = resp.send(Err(EngineError::from_detail(
                                    ErrorCode::NotFound,
                                    format!("Buffer {} not found in context {}", buffer_id, ctx_id),
                                )));
                            }
                            Err(error) => {
                                let _ = resp.send(Err(error));
                            }
                        }
                    } else {
                        let _ = resp.send(Err(EngineError::from_detail(
                            ErrorCode::NotFound,
                            format!("AudioContext {} not found", ctx_id),
                        )));
                    }
                }

                AudioCmd::CopyToChannel {
                    ctx_id,
                    buffer_id,
                    data,
                    channel,
                    start,
                    resp,
                } => {
                    if let Some(ctx) = contexts.get_mut(&ctx_id) {
                        match ctx.copy_to_channel(buffer_id, &data, channel, start) {
                            Ok(true) => {
                                let _ = resp.send(Ok(()));
                            }
                            Ok(false) => {
                                let _ = resp.send(Err(EngineError::from_detail(
                                    ErrorCode::NotFound,
                                    format!("Buffer {} channel {} not found", buffer_id, channel),
                                )));
                            }
                            Err(error) => {
                                let _ = resp.send(Err(error));
                            }
                        }
                    } else {
                        let _ = resp.send(Err(EngineError::from_detail(
                            ErrorCode::NotFound,
                            format!("AudioContext {} not found", ctx_id),
                        )));
                    }
                }

                // ==================== MediaAudioPlayer ====================
                AudioCmd::CreateMediaAudioPlayer { id } => {
                    media_players.insert(id, Vec::new());
                    tracing::debug!("Created MediaAudioPlayer {}", id);
                }

                AudioCmd::MediaAudioPlayerAddSource {
                    player_id,
                    source_id,
                } => {
                    if let Some(sources) = media_players.get_mut(&player_id) {
                        if !sources.contains(&source_id) {
                            sources.push(source_id);
                        }
                    }
                }

                AudioCmd::MediaAudioPlayerRemoveSource {
                    player_id,
                    source_id,
                } => {
                    if let Some(sources) = media_players.get_mut(&player_id) {
                        sources.retain(|&id| id != source_id);
                    }
                }

                AudioCmd::MediaAudioPlayerStart { player_id } => {
                    if let Some(sources) = media_players.get(&player_id) {
                        for &source_id in sources {
                            if let Some(player) = inner_players.get_mut(&source_id) {
                                player.play();
                            }
                        }
                    }
                }

                AudioCmd::MediaAudioPlayerStop { player_id } => {
                    if let Some(sources) = media_players.get(&player_id) {
                        for &source_id in sources {
                            if let Some(player) = inner_players.get_mut(&source_id) {
                                player.stop();
                            }
                        }
                    }
                }

                AudioCmd::MediaAudioPlayerDestroy { player_id } => {
                    media_players.remove(&player_id);
                    tracing::debug!("Destroyed MediaAudioPlayer {}", player_id);
                }

                // ==================== InnerAudioContext ====================
                AudioCmd::CreateInnerAudio { id } => {
                    if !inner_players.contains_key(&id) {
                        inner_players.insert(id, InnerAudioPlayer::new(id, channels));
                        tracing::debug!("Created InnerAudioContext {}", id);
                    }
                }

                AudioCmd::DestroyInnerAudio { id } => {
                    if inner_players.remove(&id).is_some() {
                        tracing::debug!("Destroyed InnerAudioContext {}", id);
                    }
                }

                AudioCmd::InnerAudioLoad { id, data, resp } => {
                    if inner_players.contains_key(&id) {
                        decode_pool.submit(DecodeJob::InnerAudio { id, data, resp });
                    } else {
                        let _ = resp.send(Err(EngineError::from_detail(
                            ErrorCode::NotFound,
                            format!("InnerAudioContext {} not found", id),
                        )));
                    }
                }

                AudioCmd::InnerAudioLoadUrl { id, url, resp } => {
                    if let Some(player) = inner_players.get_mut(&id) {
                        // Check cache first
                        if let Some(cached_audio) = audio_cache.get(&url) {
                            tracing::debug!("Cache hit for InnerAudioContext {}: {}", id, url);
                            player.load_cached(cached_audio);
                            let _ = resp.send(Ok(()));
                        } else {
                            match streaming_client.get() {
                                Ok(client) => {
                                    let state = StreamingState::new();
                                    let rx = streaming::start_streaming_download(
                                        client,
                                        url.clone(),
                                        state.clone(),
                                        sample_rate,
                                    );
                                    player.start_streaming(url, rx, state);
                                    let _ = resp.send(Ok(()));
                                    tracing::debug!(
                                        "Started streaming for InnerAudioContext {}: (cache miss)",
                                        id
                                    );
                                }
                                Err(error) => {
                                    let _ = resp.send(Err(error));
                                }
                            }
                        }
                    } else {
                        let _ = resp.send(Err(EngineError::from_detail(
                            ErrorCode::NotFound,
                            format!("InnerAudioContext {} not found", id),
                        )));
                    }
                }

                AudioCmd::InnerAudioPlay { id } => {
                    tracing::trace!("InnerAudioPlay command: id={}", id);
                    if let Some(player) = inner_players.get_mut(&id) {
                        player.play();
                    } else {
                        tracing::warn!("InnerAudioPlay: player {} not found", id);
                    }
                }

                AudioCmd::InnerAudioPause { id } => {
                    tracing::trace!("InnerAudioPause command: id={}", id);
                    if let Some(player) = inner_players.get_mut(&id) {
                        player.pause();
                    } else {
                        tracing::warn!("InnerAudioPause: player {} not found", id);
                    }
                }

                AudioCmd::InnerAudioStop { id } => {
                    tracing::trace!("InnerAudioStop command: id={}", id);
                    if let Some(player) = inner_players.get_mut(&id) {
                        player.stop();
                    } else {
                        tracing::warn!("InnerAudioStop: player {} not found", id);
                    }
                }

                AudioCmd::InnerAudioSeek { id, position } => {
                    tracing::trace!(
                        "InnerAudioSeek command: id={}, position={:.2}s",
                        id,
                        position
                    );
                    if let Some(player) = inner_players.get_mut(&id) {
                        player.shared.seek(position);
                    } else {
                        tracing::warn!("InnerAudioSeek: player {} not found", id);
                    }
                }

                AudioCmd::InnerAudioSetVolume { id, volume } => {
                    if let Some(player) = inner_players.get_mut(&id) {
                        player.shared.set_volume(volume);
                    }
                }

                AudioCmd::InnerAudioSetLoop { id, loop_enabled } => {
                    if let Some(player) = inner_players.get_mut(&id) {
                        player.shared.set_loop_enabled(loop_enabled);
                    }
                }

                AudioCmd::InnerAudioSetPlaybackRate { id, rate } => {
                    if let Some(player) = inner_players.get_mut(&id) {
                        player.shared.set_playback_rate(rate);
                    }
                }

                AudioCmd::InnerAudioSetAutoplay { id, autoplay } => {
                    if let Some(player) = inner_players.get_mut(&id) {
                        player.shared.set_autoplay(autoplay);
                    }
                }

                AudioCmd::InnerAudioGetState { id, resp } => {
                    if let Some(player) = inner_players.get(&id) {
                        let shared = &player.shared;
                        let state = InnerAudioState {
                            current_time: shared.current_time(),
                            duration: shared.duration(),
                            paused: shared.state() != PlaybackState::Playing,
                            volume: shared.volume(),
                            loop_enabled: shared.loop_enabled(),
                            playback_rate: shared.playback_rate(),
                            buffered: shared.is_loaded(),
                        };
                        let _ = resp.send(Ok(state));
                    } else {
                        let _ = resp.send(Err(EngineError::from_detail(
                            ErrorCode::NotFound,
                            format!("InnerAudioContext {} not found", id),
                        )));
                    }
                }

                AudioCmd::InnerAudioPollEvents { resp } => {
                    // Deprecated: polling is replaced by push mechanism
                    let _ = resp.send(Ok(Vec::new()));
                }
            }
        }

        // -----------------------------------------------------------------
        // 1b. Drain completed decode results (non-blocking).
        //     Worker threads send decoded audio back here so we can
        //     integrate the buffers without blocking the audio loop.
        // -----------------------------------------------------------------
        while let Ok(result) = decode_rx.try_recv() {
            match result {
                DecodeResult::AudioBuffer {
                    ctx_id,
                    result,
                    resp,
                } => {
                    integrate_audio_buffer_decode_result(&mut contexts, ctx_id, result, resp);
                }
                DecodeResult::InnerAudio { id, result, resp } => {
                    match result {
                        Ok(resampled) => {
                            if let Some(player) = inner_players.get_mut(&id) {
                                let info = InnerAudioInfo {
                                    duration: resampled.duration(),
                                    sample_rate: resampled.sample_rate,
                                    channels: resampled.channels,
                                };
                                player.load_audio(resampled);
                                let _ = resp.send(Ok(info));
                            } else {
                                // Player was destroyed while decode was in flight.
                                let _ = resp.send(Err(EngineError::from_detail(
                                    ErrorCode::NotFound,
                                    format!("InnerAudioContext {} destroyed during decode", id),
                                )));
                            }
                        }
                        Err(e) => {
                            let _ = resp.send(Err(e));
                        }
                    }
                }
            }
        }

        // Hitting the drain cap means the channel may still contain commands.
        // Notifications are intentionally coalesced into one latch, so the
        // loop must not block until it has observed the channel below the cap.
        let commands_may_remain = cmd_count == AUDIO_COMMANDS_PER_DRAIN;

        // -----------------------------------------------------------------
        // 2. When backgrounded, pause the hardware stream and wait for an
        //    explicit command. A failed pause retains a bounded retry tick.
        // -----------------------------------------------------------------
        if paused {
            if !output.is_alive() {
                stream_gate.mark_stopped();
            }
            if let Some(AudioStreamAction::Pause) =
                stream_gate.next_action(true, output_power.state())
            {
                if output.pause_stream() {
                    stream_gate.commit(AudioStreamAction::Pause);
                    info!("AudioThread paused (stream paused)");
                }
            }

            wait_for_audio_work(
                &wakeup,
                audio_wait_mode(
                    commands_may_remain,
                    true,
                    power.state(),
                    stream_gate.is_running(),
                    power.wait_duration(),
                    stream_retry_delay,
                ),
            );
            continue;
        }

        // -----------------------------------------------------------------
        // 3. Poll streaming data, cache completions, and push events.
        //    Combined into a single pass to avoid iterating inner_players
        //    twice (steps 3+4 merged). Step 7 (audio processing) remains
        //    separate because it runs conditionally inside a nested loop.
        // -----------------------------------------------------------------
        service_players(&mut inner_players, &audio_cache, |event| {
            tracing::trace!(
                "Pushing InnerAudio event: id={}, type={:?}, time={:.2}s",
                event.id,
                event.event_type,
                event.current_time
            );
            let ev_id = event.id;
            let ev_type = event.event_type;
            if let Err(e) = host_tx.try_send(HostCommand::InnerAudioEvent {
                id: event.id,
                event_type: event.event_type,
                current_time: event.current_time,
            }) {
                tracing::warn!(
                    "Failed to send audio event (id={}, type={:?}): {}",
                    ev_id,
                    ev_type,
                    e
                );
            }
        });

        // -----------------------------------------------------------------
        // 5. Determine management and audible-output activity independently.
        //    Single pass over players for active/streaming, and use
        //    has_active_sources() for contexts (not just Running state).
        // -----------------------------------------------------------------
        let has_active_context = contexts.values().any(|ctx| ctx.has_active_sources());

        let (mut has_active_inner, mut has_active_streaming) = (false, false);
        for p in inner_players.values() {
            if p.is_active() {
                has_active_inner = true;
            }
            if p.is_streaming() {
                has_active_streaming = true;
            }
            if has_active_inner && has_active_streaming {
                break;
            }
        }

        let output_is_active = has_active_context || has_active_inner;
        let is_active = output_is_active || has_active_streaming;

        let power_state = power.update(is_active);
        let output_power_state = output_power.update(output_is_active);

        // -----------------------------------------------------------------
        // 6. Recover dead output only when there is audible work. A newly
        //    created AudioOutput starts in play state.
        // -----------------------------------------------------------------
        if !output.is_alive() {
            stream_gate.mark_stopped();
            if output_is_active {
                match AudioOutput::new() {
                    Ok(new_output) => {
                        let recovered_rate = new_output.sample_rate();
                        let recovered_channels = new_output.channels();
                        let route_changed =
                            recovered_rate != sample_rate || recovered_channels != channels;
                        sync = new_output.sync().clone();
                        output = new_output;
                        if route_changed {
                            sample_rate = recovered_rate;
                            channels = recovered_channels;
                            process_frames = calculate_process_frames(sample_rate);
                            buffer_size = process_frames * channels as usize;
                            process_buffer.resize(buffer_size, 0.0);
                            for ctx in contexts.values_mut() {
                                ctx.renegotiate_device(sample_rate, channels);
                            }
                            // Jobs submitted after recovery use the new route
                            // rate; already-running workers finish their
                            // bounded in-flight jobs with their original target.
                            decode_pool.sample_rate = sample_rate;
                        }
                        stream_gate.mark_running();
                        recovery_delay = Duration::from_secs(1);
                        info!("AudioThread: audio output recovered after stream error");
                    }
                    Err(e) => {
                        error!(
                            "AudioThread: failed to recover audio output (retry in {:?}): {}",
                            recovery_delay, e
                        );
                        wakeup.wait_timeout(recovery_delay);
                        recovery_delay = (recovery_delay * 2).min(MAX_RECOVERY_DELAY);
                        continue;
                    }
                }
            }
        }

        let stream_action = stream_gate.next_action(false, output_power_state);
        if stream_action == Some(AudioStreamAction::Pause) && output.pause_stream() {
            stream_gate.commit(AudioStreamAction::Pause);
            info!("AudioThread entered idle sleep (stream paused)");
        }
        let resume_after_refill = stream_action == Some(AudioStreamAction::Resume);

        // -----------------------------------------------------------------
        // 7. Audio processing (only with audible work). A resume forces one
        //    refill pass before the hardware callback is restarted.
        // -----------------------------------------------------------------
        if output_is_active {
            let should_refill = resume_after_refill
                || (stream_gate.is_running() && (sync.check_and_clear() || output.needs_data()));
            if should_refill {
                // Refill only up to the high watermark, measured from the *current*
                // buffer depth (output.buffered()) rather than the stale callback
                // hint behind needs_data() — otherwise the loop would keep filling
                // until the ring is nearly full, adding ~130ms of latency before a
                // freshly triggered sound is heard.
                while output.buffered() < output.high_watermark()
                    && output.available() >= buffer_size
                {
                    process_buffer.fill(0.0);

                    // Render the block one **render quantum** at a time.
                    //
                    // The block handed to the ring is ~21 ms, sized for one
                    // efficient ring push. Rendering it in one call would make
                    // that the scheduling grain too: `start(when)` and
                    // `stop(when)` are compared against the block's start time,
                    // so a sound could land up to 21 ms from where it was asked
                    // for, and every k-rate parameter would step once per 21 ms
                    // instead of gliding. Web Audio's quantum is 128 frames, and
                    // slicing here buys that 2.67 ms grain for the cost of eight
                    // call setups per block -- the per-sample work is identical.
                    let quantum_samples = RENDER_QUANTUM_FRAMES * channels as usize;
                    for quantum in process_buffer.chunks_mut(quantum_samples) {
                        // Process WebAudio contexts, unregistering from the
                        // node→context index any node that became collectible
                        // this quantum (context.process() has already dropped it).
                        for ctx in contexts.values_mut() {
                            if ctx.state() == AudioContextState::Running {
                                for &finished_id in
                                    ctx.process_for_device(quantum, sample_rate, channels)
                                {
                                    node_index.unregister(finished_id);
                                }
                            }
                        }

                        // Process InnerAudioContext players
                        for player in inner_players.values_mut() {
                            player.process(quantum);
                        }
                    }

                    // Every contributor has now added into the block, so the
                    // limiter runs exactly once, over the complete sum.
                    soft_limit(&mut process_buffer);

                    output.write(&process_buffer);
                }
            }
        }

        if resume_after_refill && output.resume_stream() {
            stream_gate.commit(AudioStreamAction::Resume);
            info!("AudioThread resumed active output after refill");
        }

        // -----------------------------------------------------------------
        // 8. Sleep on the condvar. Stable idle is event-driven only after the
        //    output is confirmed paused; pause failures retain a retry tick.
        //
        //    Active:    5 ms — low-latency mixing
        //    LowPower: 50 ms — recently stopped, may resume
        //    Sleep:    event — explicit command wakeup
        // -----------------------------------------------------------------
        if power_state != AudioPowerState::Active {
            tracing::trace!("AudioThread power state: {:?}", power_state);
        }
        wait_for_audio_work(
            &wakeup,
            audio_wait_mode(
                commands_may_remain,
                false,
                power_state,
                stream_gate.is_running(),
                power.wait_duration(),
                stream_retry_delay,
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    use shared::protocol::host_cmd::InnerAudioEventType;

    /// The limiter must not introduce an edge of its own. The previous curve left
    /// `1.0` untouched and sent `1.0 + eps` to `0.5`: a 6 dB discontinuity right
    /// where signals cross it, heard as a click on every overshoot.
    #[test]
    fn the_soft_limit_is_continuous_and_bounded() {
        let mut probe: Vec<f32> = (0..4_000).map(|i| i as f32 * 0.005 - 10.0).collect();
        soft_limit(&mut probe);

        for window in probe.windows(2) {
            let step = (window[1] - window[0]).abs();
            assert!(
                step < 0.01,
                "limiter jumped by {step} between adjacent inputs"
            );
        }
        assert!(
            probe.iter().all(|s| s.abs() <= 1.0),
            "the limiter must keep the mix inside [-1, 1]"
        );
    }

    #[test]
    fn the_soft_limit_leaves_signals_below_the_knee_untouched() {
        let quiet: Vec<f32> = (0..64).map(|i| (i as f32 / 64.0) * 1.5 - 0.75).collect();
        let mut limited = quiet.clone();
        soft_limit(&mut limited);
        assert_eq!(limited, quiet, "must be transparent inside the knee");
    }

    /// Applying the limiter per contributor made the result depend on how many
    /// contributors there were and on `HashMap` order. One pass over a finished
    /// sum is idempotent-in-shape: limiting a sum must not depend on how it was
    /// accumulated.
    #[test]
    fn the_soft_limit_depends_only_on_the_finished_sum() {
        let mut one_shot = vec![0.4f32, -1.9, 1.2];
        let mut accumulated = vec![0.0f32; 3];
        for part in [[0.1f32, -1.0, 0.5], [0.3, -0.9, 0.7]] {
            for (dst, src) in accumulated.iter_mut().zip(part.iter()) {
                *dst += src;
            }
        }

        soft_limit(&mut one_shot);
        soft_limit(&mut accumulated);

        for (a, b) in one_shot.iter().zip(accumulated.iter()) {
            assert!((a - b).abs() < 1e-6, "{a} != {b}");
        }
    }

    /// The ring block is sliced into render quanta, so a block size that is not a
    /// whole number of quanta would render a short final quantum every block --
    /// a periodic hitch in every source's scheduling grain.
    #[test]
    fn every_ring_block_is_a_whole_number_of_render_quanta() {
        for sample_rate in [
            8_000, 16_000, 22_050, 32_000, 44_100, 48_000, 96_000, 192_000,
        ] {
            let frames = calculate_process_frames(sample_rate);
            assert_eq!(
                frames % RENDER_QUANTUM_FRAMES,
                0,
                "{sample_rate} Hz gives a {frames}-frame block, not a multiple of the quantum"
            );
            assert!(frames >= RENDER_QUANTUM_FRAMES);
        }
    }

    fn inner_decode_job(
        id: InnerAudioId,
    ) -> (
        DecodeJob,
        tokio::sync::oneshot::Receiver<EngineResult<InnerAudioInfo>>,
    ) {
        let (resp, rx) = tokio::sync::oneshot::channel();
        (
            DecodeJob::InnerAudio {
                id,
                data: vec![0],
                resp,
            },
            rx,
        )
    }

    fn audio_buffer_decode_job(
        ctx_id: AudioContextId,
        data: Arc<Vec<u8>>,
    ) -> (
        DecodeJob,
        tokio::sync::oneshot::Receiver<EngineResult<AudioBufferInfo>>,
    ) {
        let (resp, rx) = tokio::sync::oneshot::channel();
        (DecodeJob::AudioBuffer { ctx_id, data, resp }, rx)
    }

    fn failed_decode_result(id: InnerAudioId) -> DecodeResult {
        let (resp, _rx) = tokio::sync::oneshot::channel();
        DecodeResult::InnerAudio {
            id,
            result: Err(EngineError::new(ErrorCode::InvalidArgument)),
            resp,
        }
    }

    fn successful_decode_result(
        id: InnerAudioId,
        samples: Vec<f32>,
    ) -> (
        DecodeResult,
        tokio::sync::oneshot::Receiver<EngineResult<InnerAudioInfo>>,
    ) {
        let (resp, response) = tokio::sync::oneshot::channel();
        (
            DecodeResult::InnerAudio {
                id,
                result: Ok(DecodedAudio {
                    samples,
                    sample_rate: 48_000,
                    channels: 1,
                }),
                resp,
            },
            response,
        )
    }

    fn small_in_flight_budget(
        max_bytes: usize,
        reservation_bytes: usize,
    ) -> Arc<DecodeInFlightUsage> {
        Arc::new(DecodeInFlightUsage::new(max_bytes, reservation_bytes))
    }

    #[test]
    fn in_flight_budget_rejects_the_first_reservation_over_its_limit() {
        let budget = small_in_flight_budget(2, 1);
        let first = budget.try_reserve().expect("first reservation");
        let second = budget.try_reserve().expect("exact limit reservation");
        assert!(budget.try_reserve().is_none(), "limit + 1 must be rejected");
        drop(first);
        drop(second);
        assert_eq!(budget.bytes.load(std::sync::atomic::Ordering::Acquire), 0);
    }

    #[test]
    fn in_flight_budget_allows_only_the_configured_concurrent_workers() {
        let budget = small_in_flight_budget(2, 1);
        let barrier = Arc::new(std::sync::Barrier::new(4));
        let mut workers = Vec::new();
        for _ in 0..3 {
            let budget = Arc::clone(&budget);
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                barrier.wait();
                let permit = budget.try_reserve();
                barrier.wait();
                permit.is_some()
            }));
        }
        barrier.wait();
        barrier.wait();
        let admitted = workers
            .into_iter()
            .map(|worker| worker.join().unwrap() as usize)
            .sum::<usize>();
        assert_eq!(admitted, 2, "the third concurrent worker must be refused");
        assert_eq!(budget.bytes.load(std::sync::atomic::Ordering::Acquire), 0);
    }

    #[test]
    fn in_flight_budget_is_returned_after_error_panic_and_drop() {
        let budget = small_in_flight_budget(1, 1);
        let error_result: Result<(), ()> = {
            let _permit = budget.try_reserve().expect("error path reservation");
            Err(())
        };
        assert!(error_result.is_err());
        assert_eq!(budget.bytes.load(std::sync::atomic::Ordering::Acquire), 0);

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe({
            let budget = Arc::clone(&budget);
            move || {
                let _permit = budget.try_reserve().expect("panic path reservation");
                panic!("simulated decoder panic");
            }
        }));
        assert!(panicked.is_err());
        assert_eq!(budget.bytes.load(std::sync::atomic::Ordering::Acquire), 0);

        let permit = budget.try_reserve().expect("drop path reservation");
        drop(permit);
        assert_eq!(budget.bytes.load(std::sync::atomic::Ordering::Acquire), 0);
    }

    #[test]
    fn decoder_panic_is_converted_to_an_internal_error_instead_of_exiting_the_worker() {
        let result = run_decode_with_panic_boundary(|| -> EngineResult<DecodedAudio> {
            panic!("simulated malicious decoder panic")
        });

        let error = result.expect_err("decoder panic must become a normal result");
        assert_eq!(error.code, ErrorCode::Internal);
    }

    #[test]
    fn separately_constructed_decode_pools_share_the_process_in_flight_budget() {
        let (result_tx, _result_rx) = decode_queue(1, MAX_DECODE_RESULT_QUEUED_BYTES);
        let first = DecodePool::new(0, result_tx.clone(), 48_000, ThreadWakeup::new());
        let second = DecodePool::new(0, result_tx, 48_000, ThreadWakeup::new());

        assert!(
            Arc::ptr_eq(&first.in_flight, &second.in_flight),
            "all production decode pools must use one process-wide budget"
        );
    }

    #[test]
    fn shared_decode_budget_rejects_a_third_job_and_raii_releases_it_for_another_pool() {
        let budget = small_in_flight_budget(2, 1);
        let first_pool_budget = Arc::clone(&budget);
        let second_pool_budget = Arc::clone(&budget);
        let first = first_pool_budget.try_reserve().unwrap();
        let second = second_pool_budget.try_reserve().unwrap();

        assert!(
            budget.try_reserve().is_none(),
            "two pools together must share the limit"
        );
        drop(first);
        assert!(
            second_pool_budget.try_reserve().is_some(),
            "dropping one pool's permit must return capacity to the other"
        );
        drop(second);
        assert_eq!(budget.bytes.load(std::sync::atomic::Ordering::Acquire), 0);
    }

    #[test]
    fn decode_job_and_result_transports_are_bounded_and_nonblocking() {
        let source = include_str!("audio_thread.rs");

        assert!(
            !source.contains(concat!("std_mpsc::", "channel::<PoolMsg>()")),
            "decode jobs must not accumulate in an unbounded queue"
        );
        assert!(
            !source.contains(concat!("std_mpsc::", "channel::<DecodeResult>()")),
            "decode results must not accumulate in an unbounded queue"
        );
        assert!(
            source.contains(concat!("job_tx.", "try_send"))
                && source.contains(concat!("result_tx.", "try_send")),
            "both producers must report Full without blocking the audio thread"
        );
    }

    #[test]
    fn every_decode_reserves_the_shared_in_flight_peak_budget_before_decoding() {
        let source = include_str!("audio_thread.rs");
        let worker = source
            .find("fn decode_worker(")
            .expect("decode worker implementation");
        let worker_body = &source[worker..source.find("use crate::cache").unwrap()];
        let reserve = worker_body.find("in_flight.try_reserve()");
        let decode = worker_body.find("crate::decoder::decode");

        assert!(
            reserve.is_some(),
            "worker must reserve the shared peak budget"
        );
        assert!(
            reserve.unwrap() < decode.unwrap(),
            "peak admission must happen before the decoder allocates"
        );
        assert!(
            source.contains("process_decode_in_flight_budget()"),
            "the pool must use the process-wide in-flight budget"
        );
    }

    #[test]
    fn a_full_decode_job_queue_replies_with_input_saturated() {
        let (job_tx, _job_rx) = decode_queue(1, MAX_DECODE_JOB_QUEUED_BYTES);
        job_tx
            .try_send(inner_decode_job(1).0)
            .expect("fixture fills the only job slot");
        let pool = DecodePool {
            job_tx: Some(job_tx),
            in_flight: small_in_flight_budget(
                MAX_DECODE_IN_FLIGHT_BYTES,
                DECODE_IN_FLIGHT_RESERVATION_BYTES,
            ),
            workers: Vec::new(),
        };
        let (job, mut response) = inner_decode_job(2);

        pool.submit(job);

        let error = response
            .try_recv()
            .expect("full admission must resolve the response immediately")
            .expect_err("the full queue must reject the decode");
        assert_eq!(error.code, ErrorCode::InputSaturated);
    }

    #[test]
    fn decode_job_queue_rejects_limit_plus_one_encoded_byte() {
        const EXPECTED_BYTE_LIMIT: usize = 64 * 1024 * 1024;
        const CHUNK_BYTES: usize = 16 * 1024 * 1024;

        let (job_tx, _job_rx) =
            decode_queue(DECODE_JOB_QUEUE_CAPACITY, MAX_DECODE_JOB_QUEUED_BYTES);
        let pool = DecodePool {
            job_tx: Some(job_tx),
            in_flight: small_in_flight_budget(
                MAX_DECODE_IN_FLIGHT_BYTES,
                DECODE_IN_FLIGHT_RESERVATION_BYTES,
            ),
            workers: Vec::new(),
        };
        let data = Arc::new(vec![0; CHUNK_BYTES]);
        for ctx_id in 0..(EXPECTED_BYTE_LIMIT / CHUNK_BYTES) as u32 {
            pool.submit(audio_buffer_decode_job(ctx_id, data.clone()).0);
        }
        let (job, mut response) = audio_buffer_decode_job(99, Arc::new(vec![0]));

        pool.submit(job);

        let error = response
            .try_recv()
            .expect("limit + 1 byte must be refused immediately")
            .expect_err("the encoded byte backlog must be bounded");
        assert_eq!(error.code, ErrorCode::InputSaturated);
    }

    #[test]
    fn a_stopped_decode_job_receiver_replies_with_disconnected() {
        let (job_tx, job_rx) = decode_queue(1, MAX_DECODE_JOB_QUEUED_BYTES);
        drop(job_rx);
        let pool = DecodePool {
            job_tx: Some(job_tx),
            in_flight: small_in_flight_budget(
                MAX_DECODE_IN_FLIGHT_BYTES,
                DECODE_IN_FLIGHT_RESERVATION_BYTES,
            ),
            workers: Vec::new(),
        };
        let (job, mut response) = inner_decode_job(3);

        pool.submit(job);

        let error = response
            .try_recv()
            .expect("disconnection must resolve the response immediately")
            .expect_err("the stopped receiver must reject the decode");
        assert_eq!(error.code, ErrorCode::Disconnected);
    }

    #[test]
    fn a_full_decode_result_queue_replies_without_blocking_the_worker() {
        let (result_tx, _result_rx) = decode_queue(1, MAX_DECODE_RESULT_QUEUED_BYTES);
        result_tx
            .try_send(failed_decode_result(10))
            .expect("fixture fills the only result slot");
        let (job_tx, job_rx) = decode_queue(1, MAX_DECODE_JOB_QUEUED_BYTES);
        let (job, mut response) = inner_decode_job(11);
        job_tx.try_send(job).unwrap();
        drop(job_tx);

        decode_worker(
            Arc::new(std::sync::Mutex::new(job_rx)),
            result_tx,
            small_in_flight_budget(
                MAX_DECODE_IN_FLIGHT_BYTES,
                DECODE_IN_FLIGHT_RESERVATION_BYTES,
            ),
            48_000,
            ThreadWakeup::new(),
        );

        let error = response
            .try_recv()
            .expect("full publication must resolve the response immediately")
            .expect_err("the full result queue must reject the decode");
        assert_eq!(error.code, ErrorCode::InputSaturated);
    }

    #[test]
    fn a_stopped_decode_result_receiver_replies_with_disconnected() {
        let (result_tx, result_rx) = decode_queue(1, MAX_DECODE_RESULT_QUEUED_BYTES);
        drop(result_rx);
        let (job_tx, job_rx) = decode_queue(1, MAX_DECODE_JOB_QUEUED_BYTES);
        let (job, mut response) = inner_decode_job(12);
        job_tx.try_send(job).unwrap();
        drop(job_tx);

        decode_worker(
            Arc::new(std::sync::Mutex::new(job_rx)),
            result_tx,
            small_in_flight_budget(
                MAX_DECODE_IN_FLIGHT_BYTES,
                DECODE_IN_FLIGHT_RESERVATION_BYTES,
            ),
            48_000,
            ThreadWakeup::new(),
        );

        let error = response
            .try_recv()
            .expect("disconnection must resolve the response immediately")
            .expect_err("the stopped result receiver must reject the decode");
        assert_eq!(error.code, ErrorCode::Disconnected);
    }

    #[test]
    fn decode_result_queue_rejects_limit_plus_one_pcm_byte_and_releases_on_receive() {
        let (result_tx, result_rx) = decode_queue(2, std::mem::size_of::<f32>());
        let (at_limit, _first_response) = successful_decode_result(20, vec![0.0]);
        publish_decode_result(&result_tx, at_limit);

        let (over_limit, mut response) = successful_decode_result(21, vec![1.0]);
        publish_decode_result(&result_tx, over_limit);

        let error = response
            .try_recv()
            .expect("limit + 1 PCM payload must be refused immediately")
            .expect_err("the result byte backlog must be bounded");
        assert_eq!(error.code, ErrorCode::InputSaturated);
        assert_eq!(
            result_tx
                .usage
                .bytes
                .load(std::sync::atomic::Ordering::Acquire),
            std::mem::size_of::<f32>()
        );

        assert!(matches!(
            result_rx.try_recv(),
            Ok(DecodeResult::InnerAudio { id: 20, .. })
        ));
        assert_eq!(
            result_tx
                .usage
                .bytes
                .load(std::sync::atomic::Ordering::Acquire),
            0
        );
    }

    #[test]
    fn decode_receiver_exit_releases_queued_permits() {
        let (job_tx, job_rx) = decode_queue(2, 8);
        job_tx.try_send(inner_decode_job(30).0).unwrap();
        assert_eq!(
            job_tx
                .usage
                .bytes
                .load(std::sync::atomic::Ordering::Acquire),
            1
        );

        let stopped = thread::spawn(move || {
            drop(job_rx);
            panic!("simulated decode worker exit");
        });
        assert!(stopped.join().is_err());
        assert_eq!(
            job_tx
                .usage
                .items
                .load(std::sync::atomic::Ordering::Acquire),
            0
        );
        assert_eq!(
            job_tx
                .usage
                .bytes
                .load(std::sync::atomic::Ordering::Acquire),
            0
        );
    }

    fn create_context(ctx_id: u32) -> AudioCmd {
        AudioCmd::CreateContext {
            ctx_id,
            sample_rate: None,
        }
    }

    #[test]
    fn release_buffer_is_idempotent_and_never_scans_other_contexts() {
        let mut contexts = HashMap::new();
        let mut first = AudioContext::new(1, 48_000, 2);
        let mut second = AudioContext::new(2, 48_000, 2);
        let first_id = first.create_empty_buffer(1, 1, 48_000).unwrap();
        let second_id = second.create_empty_buffer(1, 1, 48_000).unwrap();
        assert_eq!(first_id, second_id, "fixture requires colliding local ids");
        contexts.insert(1, first);
        contexts.insert(2, second);

        release_buffer_scoped(&mut contexts, 1, first_id);
        release_buffer_scoped(&mut contexts, 1, first_id);

        assert!(contexts.get(&1).unwrap().get_buffer(first_id).is_none());
        assert!(contexts.get(&2).unwrap().get_buffer(second_id).is_some());
    }

    #[test]
    fn release_context_is_idempotent_and_clears_the_node_index() {
        let mut contexts = HashMap::new();
        let mut context = AudioContext::new(7, 48_000, 2);
        context.create_buffer_source(70);
        contexts.insert(7, context);
        let mut node_index = NodeContextIndex::new();
        node_index.register(70, 7);

        assert!(release_context_scoped(&mut contexts, &mut node_index, 7));
        assert!(!contexts.contains_key(&7));
        assert_eq!(node_index.get_context(70), None);

        node_index.register(71, 7);
        assert!(!release_context_scoped(&mut contexts, &mut node_index, 7));
        assert_eq!(node_index.get_context(71), None);
    }

    #[test]
    fn release_all_barrier_discards_old_backlog_and_full_queue_before_ack() {
        let (tx, rx) = shared::audio_channel::channel();
        for ctx_id in 100..100 + shared::audio_channel::AUDIO_COMMAND_CAPACITY as u32 {
            tx.try_send(create_context(ctx_id))
                .expect("fixture fills the ordinary data queue");
        }
        let ticket = tx
            .request_release_all_contexts()
            .expect("cleanup bypasses the full data queue");

        let (close_resp, mut close_rx) = tokio::sync::oneshot::channel();
        let mut startup_backlog = vec![
            create_context(1),
            AudioCmd::CloseContext {
                ctx_id: 1,
                resp: close_resp,
            },
        ]
        .into_iter();
        let mut contexts = HashMap::new();
        let mut first = AudioContext::new(1, 48_000, 2);
        first.create_buffer_source(11);
        contexts.insert(1, first);
        contexts.insert(2, AudioContext::new(2, 48_000, 2));
        let mut node_index = NodeContextIndex::new();
        node_index.register(11, 1);
        node_index.register(22, 2);

        let barrier = next_command(&mut startup_backlog, &rx)
            .expect("cleanup must interrupt old startup data");
        assert!(matches!(barrier, AudioCmd::ReleaseAllContexts));
        handle_release_all_contexts(&mut startup_backlog, &rx, &mut contexts, &mut node_index);

        assert!(contexts.is_empty());
        assert_eq!(node_index.get_context(11), None);
        assert_eq!(node_index.get_context(22), None);
        assert!(startup_backlog.next().is_none());
        assert!(rx.is_empty(), "all old ordinary commands must be discarded");
        assert!(matches!(
            close_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ));
        assert!(ticket.is_complete(), "ack is last");

        while let Some(cmd) = next_command(&mut startup_backlog, &rx) {
            if let AudioCmd::CreateContext {
                ctx_id,
                sample_rate,
            } = cmd
            {
                create_context_scoped(&mut contexts, ctx_id, sample_rate.unwrap_or(48_000), 2);
            }
        }
        assert!(
            contexts.is_empty(),
            "discarded pre-barrier creates must never rebuild contexts after ack"
        );
    }

    #[test]
    fn duplicate_create_context_is_a_no_op_that_preserves_the_original() {
        let mut contexts = HashMap::new();

        assert!(create_context_scoped(&mut contexts, 9, 44_100, 2));
        assert!(!create_context_scoped(&mut contexts, 9, 48_000, 1));

        let context = contexts.get(&9).unwrap();
        assert_eq!(context.sample_rate(), 44_100);
        assert_eq!(context.channels(), 2);
    }

    #[test]
    fn abandoned_audio_buffer_decode_result_never_enters_same_numeric_context() {
        let mut contexts = HashMap::new();
        contexts.insert(12, AudioContext::new(12, 48_000, 2));
        let (resp, receiver) = tokio::sync::oneshot::channel();
        drop(receiver);
        let decoded = DecodedAudio {
            samples: vec![0.25, -0.25],
            sample_rate: 48_000,
            channels: 2,
        };

        integrate_audio_buffer_decode_result(&mut contexts, 12, Ok(decoded), resp);

        assert_eq!(contexts.get(&12).unwrap().buffer_channels(1), None);
    }

    #[test]
    fn live_audio_buffer_decode_result_is_inserted_and_replied() {
        let mut contexts = HashMap::new();
        contexts.insert(13, AudioContext::new(13, 48_000, 2));
        let (resp, mut receiver) = tokio::sync::oneshot::channel();
        let decoded = DecodedAudio {
            samples: vec![0.25, -0.25],
            sample_rate: 48_000,
            channels: 2,
        };

        integrate_audio_buffer_decode_result(&mut contexts, 13, Ok(decoded), resp);

        let info = receiver
            .try_recv()
            .expect("live receiver gets completion")
            .expect("decode insertion succeeds");
        assert_eq!(info.id, 1);
        assert_eq!(contexts.get(&13).unwrap().buffer_channels(info.id), Some(2));
    }

    #[test]
    fn set_buffer_rejects_claimed_context_that_does_not_own_node() {
        let mut contexts = HashMap::new();
        let mut first = AudioContext::new(1, 48_000, 2);
        let mut second = AudioContext::new(2, 48_000, 2);
        first.create_buffer_source(10);
        let first_buffer = first.create_empty_buffer(1, 1, 48_000).unwrap();
        let second_buffer = second.create_empty_buffer(1, 1, 48_000).unwrap();
        assert_eq!(
            first_buffer, second_buffer,
            "fixture requires colliding local ids"
        );
        contexts.insert(1, first);
        contexts.insert(2, second);
        let mut node_index = NodeContextIndex::new();
        node_index.register(10, 1);

        assert!(!set_buffer_scoped(
            &mut contexts,
            &node_index,
            2,
            10,
            Some(second_buffer),
        ));
        assert!(set_buffer_scoped(
            &mut contexts,
            &node_index,
            1,
            10,
            Some(first_buffer),
        ));
        assert!(set_buffer_scoped(&mut contexts, &node_index, 1, 10, None,));
    }

    #[test]
    fn snapshot_buffer_commands_validate_context_and_node_ownership() {
        let mut contexts = HashMap::new();
        let mut first = AudioContext::new(1, 48_000, 2);
        first.create_buffer_source(10);
        contexts.insert(1, first);
        let mut node_index = NodeContextIndex::new();
        node_index.register(10, 1);

        assert!(!set_started_buffer_scoped(
            &mut contexts,
            &node_index,
            2,
            10,
            None,
        ));
        assert!(set_started_buffer_scoped(
            &mut contexts,
            &node_index,
            1,
            10,
            None,
        ));

        let mut contexts = HashMap::new();
        let mut context = AudioContext::new(3, 48_000, 2);
        context.create_buffer_source(30);
        contexts.insert(3, context);
        let mut node_index = NodeContextIndex::new();
        node_index.register(30, 3);
        assert!(start_buffer_scoped(
            &mut contexts,
            &node_index,
            3,
            30,
            None,
            0.0,
            0.0,
            None,
        ));
    }

    fn context_id(cmd: &AudioCmd) -> u32 {
        match cmd {
            AudioCmd::CreateContext { ctx_id, .. } => *ctx_id,
            _ => panic!("fixture only builds CreateContext"),
        }
    }

    /// A command the service accepted before this thread existed must run before
    /// one that is still in the channel, and every command must run exactly once.
    ///
    /// **This is the property that replaced re-injecting the backlog into the
    /// channel**, which the bounded transport made impossible: at the moment the
    /// service hands the receiver over, nothing is draining the queue, so putting
    /// commands back into a full one would park the caller forever. It also
    /// removes an ordering argument that was never sound — the game thread can
    /// enqueue while the handover runs, and a re-injected command would then land
    /// behind a newer one.
    #[test]
    fn the_startup_backlog_runs_before_anything_still_in_the_channel() {
        let (tx, rx) = shared::audio_channel::channel();
        tx.try_send(create_context(30)).unwrap();
        tx.try_send(create_context(40)).unwrap();
        let mut backlog = vec![create_context(10), create_context(20)].into_iter();

        let mut seen = Vec::new();
        while let Some(cmd) = next_command(&mut backlog, &rx) {
            seen.push(context_id(&cmd));
        }

        assert_eq!(
            seen,
            vec![10, 20, 30, 40],
            "the backlog and the channel were interleaved or reordered"
        );
    }

    /// With nothing buffered the drain is the channel alone, which is every
    /// iteration after the first.
    #[test]
    fn an_empty_backlog_leaves_the_channel_order_untouched() {
        let (tx, rx) = shared::audio_channel::channel();
        tx.try_send(create_context(1)).unwrap();
        tx.try_send(create_context(2)).unwrap();
        let mut backlog = Vec::new().into_iter();

        let mut seen = Vec::new();
        while let Some(cmd) = next_command(&mut backlog, &rx) {
            seen.push(context_id(&cmd));
        }

        assert_eq!(seen, vec![1, 2]);
    }

    #[test]
    fn decode_pool_starts_only_when_first_job_is_submitted() {
        let (result_tx, result_rx) =
            decode_queue(DECODE_RESULT_QUEUE_CAPACITY, MAX_DECODE_RESULT_QUEUED_BYTES);
        let mut pool = LazyDecodePool::new(result_tx, 48_000, ThreadWakeup::new());

        assert!(!pool.is_started());

        let (resp, _resp_rx) = tokio::sync::oneshot::channel();
        pool.submit(DecodeJob::InnerAudio {
            id: 7,
            data: Vec::new(),
            resp,
        });

        assert!(pool.is_started());
        assert!(result_rx.recv_timeout(Duration::from_secs(1)).is_ok());
    }

    /// Section 7.3's steady-state allocation gate, applied to the audio thread's
    /// own tick.
    ///
    /// The tick is what runs 200 times a second while anything is audible, so a
    /// per-tick allocation is a per-5ms allocation on the thread that must never
    /// be late. Three pieces of steady work are covered here and nowhere else:
    /// draining what the network delivered (`poll_stream`), mixing one block
    /// (`process`), and handing out the events the block raised.
    ///
    /// **The events are the point.** A block that crosses the `TimeUpdate`
    /// throttle raises one, and every player raises one roughly four times a
    /// second forever. The graph gate in `context.rs` cannot see this: it renders
    /// a quantum, and `InnerAudioPlayer` is the other half of the mixer.
    ///
    /// The warm-up covers the source vector's first fill and the stream
    /// receiver's first poll. Every iteration then delivers exactly one chunk and
    /// consumes exactly one block, so the buffered depth is stationary and the
    /// measured window is genuine steady state rather than a drain.
    #[test]
    fn a_steady_state_audio_thread_tick_never_reaches_the_heap() {
        const OUTPUT_CHANNELS: u32 = 2;
        const SAMPLE_RATE: u32 = 48_000;
        // One block per tick, and one chunk in per block out, so nothing drifts.
        const BLOCK_FRAMES: usize = 1024;
        const CHUNK_SAMPLES: usize = BLOCK_FRAMES * OUTPUT_CHANNELS as usize;

        let cache = GlobalAudioCache::new();
        let mut players: HashMap<InnerAudioId, InnerAudioPlayer> = HashMap::with_capacity(1);

        // A bounded channel models the decoder's one-chunk-at-a-time feed. The
        // previous fixture queued the whole burst before measuring, so its first
        // poll drained everything and the measured ticks no longer represented
        // continuous production.
        let (tx, rx) = tokio::sync::mpsc::channel::<streaming::StreamMsg>(2);
        let state = StreamingState::new();

        let mut player = InnerAudioPlayer::new(1, OUTPUT_CHANNELS);
        player.start_streaming("http://example/track.mp3".into(), rx, state);
        player.shared.set_sample_rate(SAMPLE_RATE);
        player.shared.set_channels(OUTPUT_CHANNELS);
        player.shared.set_loaded(true);
        player.shared.set_state(PlaybackState::Playing);
        players.insert(1, player);

        // Feed from a helper thread. Tokio's bounded channel allocates a queue
        // node for each send; that is producer-side work, not work on the PCM
        // deadline path. A synchronous handoff keeps one chunk in flight while
        // the allocator gate measures only service/mix/event processing.
        let (feed_tx, feed_rx) = std_mpsc::sync_channel::<()>(1);
        let (feed_ack_tx, feed_ack_rx) = std_mpsc::sync_channel::<()>(1);
        let feeder = thread::spawn(move || {
            let mut pool = streaming::PcmPool::new();
            while feed_rx.recv().is_ok() {
                let mut pcm = pool.take();
                pcm.buffer_mut().resize(CHUNK_SAMPLES, 0.0);
                if tx.try_send(streaming::StreamMsg::Samples(pcm)).is_err() {
                    break;
                }
                if feed_ack_tx.send(()).is_err() {
                    break;
                }
            }
        });
        let mut block = vec![0.0f32; BLOCK_FRAMES * OUTPUT_CHANNELS as usize];
        let mut events = 0usize;

        // Keep feeding one chunk and servicing it for the whole warm-up and
        // measured windows. Queue/loan return allocations belong to the
        // producer handoff; the deadline gate below isolates the PCM producer's
        // actual mix operation from those channel-node allocations.
        for _ in 0..80 {
            feed_tx.send(()).expect("the feeder must stay alive");
            feed_ack_rx
                .recv()
                .expect("the feeder must deliver one chunk");
            service_players(&mut players, &cache, |_| events += 1);
            block.fill(0.0);
            for player in players.values_mut() {
                player.process(&mut block);
            }
        }

        assert_no_steady_state_allocation(
            Burst {
                path: "audio: steady-state PCM mix after continuous feed",
                warmup: 8,
                measured: 64,
            },
            |_| {
                block.fill(0.0);
                for player in players.values_mut() {
                    player.process(&mut block);
                }
            },
        );
        drop(feed_tx);
        feeder.join().expect("feed thread must exit");

        assert!(
            events > 0,
            "the burst must have raised player events, or it proves nothing about emitting them"
        );
    }
    #[test]
    fn completed_stream_handoff_preserves_next_sample_and_play_state() {
        let mut players = HashMap::new();
        let cache = GlobalAudioCache::new();
        let state = StreamingState::new();
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let mut player = InnerAudioPlayer::new(1, 1);
        player.shared.set_autoplay(true);
        player.start_streaming("http://example/track.mp3".into(), rx, state);
        players.insert(1, player);

        let mut pool = streaming::PcmPool::new();
        let mut pcm = pool.take();
        pcm.buffer_mut().resize(24_000, 0.5);
        tx.try_send(streaming::StreamMsg::Ready {
            sample_rate: 48_000,
            channels: 1,
        })
        .unwrap();
        tx.try_send(streaming::StreamMsg::Samples(pcm)).unwrap();

        let mut first_events = Vec::new();
        service_players(&mut players, &cache, |event| {
            first_events.push(event.event_type)
        });
        assert_eq!(players[&1].shared.state(), PlaybackState::Playing);
        let mut first_sample = [0.0f32; 1];
        players.get_mut(&1).unwrap().process(&mut first_sample);
        assert_eq!(players[&1].shared.position_frames(), 1);

        tx.try_send(streaming::StreamMsg::Done).unwrap();
        let mut done_events = Vec::new();
        service_players(&mut players, &cache, |event| {
            done_events.push(event.event_type)
        });
        assert_eq!(players[&1].shared.state(), PlaybackState::Playing);
        assert_eq!(players[&1].shared.position_frames(), 1);
        assert!(
            done_events.is_empty(),
            "Done handoff must not replay lifecycle events"
        );
        assert!(
            first_events.contains(&InnerAudioEventType::CanPlay)
                && first_events.contains(&InnerAudioEventType::Play)
        );
    }

    #[test]
    fn worker_joins_share_one_total_deadline() {
        let handles = vec![
            thread::spawn(|| thread::sleep(Duration::from_secs(1))),
            thread::spawn(|| thread::sleep(Duration::from_secs(1))),
        ];
        let started = std::time::Instant::now();

        join_all_with_timeout(handles, Duration::from_millis(100), "test-worker");

        assert!(
            started.elapsed() < Duration::from_millis(175),
            "worker timeouts must share one total deadline"
        );
    }
}
