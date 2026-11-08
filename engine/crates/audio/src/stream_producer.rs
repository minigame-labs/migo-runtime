//! One stream's producer: a blocking PCM source on its own thread, the
//! streaming SRC on the source's own clock, and the publication / control
//! protocol the audio-thread consumer speaks (`stream_control`).
//!
//! The thread blocks where the work blocks: on source IO, on a full PCM
//! channel (backpressure) and on the consumer's accept of a reposition. While
//! it waits for channel room it still serves controls, so a paused player can
//! seek. Repositions keep the global SRC grid: decoding restarts on a grid
//! period at least the filter's history before the target, and the SRC drops
//! the preroll, so a seek or loop publishes the samples continuous playback
//! would have.
use crate::pcm_source::PcmSource;
use crate::resample_job::bandlimited::Progress;
use crate::stream_control::{Command, LoopRequest, Prepared, Reply};
use crate::streaming::{
    PcmChunk, PcmPool, PendingPublication, StreamCutover, StreamMsg, StreamPresentation,
    StreamPublication, StreamRevision, StreamTimeline, StreamTraversal, StreamingState,
};
use crate::streaming_sinc::{STEP_SAMPLES, Stream as Sinc};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

/// Opens the source on the producer thread (blocking IO happens there).
pub(crate) type Open =
    Box<dyn FnOnce(&Arc<StreamingState>) -> EngineResult<Box<dyn PcmSource>> + Send>;

/// Where a file load records the typed cause of a failure before the consumer
/// reads the message.
pub(crate) type ErrorCell = Arc<std::sync::Mutex<Option<EngineError>>>;

/// Decoders and the SRC keep their state on the heap; this is ample.
const STACK_BYTES: usize = 1 << 20;

fn invalid(detail: &str) -> EngineError {
    EngineError::from_detail(ErrorCode::IoError, detail)
}

/// Start a producer. Its failures reach the consumer as `StreamMsg::Error`;
/// the thread ends with the stream (dropping the handle detaches it).
pub(crate) fn spawn(
    open: Open,
    target: u32,
    tx: mpsc::Sender<StreamMsg>,
    state: Arc<StreamingState>,
    error_cell: Option<ErrorCell>,
) -> EngineResult<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("audio-decode".into())
        .stack_size(STACK_BYTES)
        .spawn(move || {
            let runtime = crate::streaming::get_stream_runtime().handle().clone();
            let result = run(open, target, &tx, &state, &runtime);
            if let Err((revision, error)) = result
                && !state.is_cancelled()
            {
                if let Some(cell) = &error_cell {
                    *cell.lock().unwrap_or_else(|p| p.into_inner()) = Some(error.clone());
                }
                // The consumer drops messages of retired revisions: a failure
                // belongs to the revision it last accepted.
                let message = StreamMsg::Error {
                    revision,
                    message: error.to_string(),
                };
                let _ = runtime.block_on(async {
                    tokio::select! {
                        biased;
                        _ = state.cancelled() => Err(()),
                        sent = tx.send(message) => sent.map_err(|_| ()),
                    }
                });
            }
        })
        .map_err(|error| EngineError::from_detail(ErrorCode::OutOfMemory, error.to_string()))
}

/// What one traversal of the source still has to deliver.
struct Traversal {
    sinc: Sinc,
    channels: usize,
    /// Decoded source frames not yet taken by the SRC.
    native: Vec<f32>,
    head: usize,
    /// SRC output not yet published.
    out: Vec<f32>,
    /// Playable frames of one pass, when known.
    frames: Option<u64>,
    /// Frames of the current pass already decoded.
    local: u64,
    /// Passes to play in total (`None`: endless loop) and passes finished.
    cycles: Option<u64>,
    completed: u64,
    /// No more input will reach the SRC.
    input_done: bool,
    /// The SRC has emitted its last frame.
    finished: bool,
}

impl Traversal {
    fn new(
        sinc: Sinc,
        channels: u32,
        frames: Option<u64>,
        origin: u64,
        cycles: Option<u64>,
    ) -> Self {
        let (local, completed) = match frames {
            Some(n) => (origin % n, origin / n),
            None => (0, 0),
        };
        Self {
            sinc,
            channels: channels as usize,
            native: Vec::new(),
            head: 0,
            out: Vec::with_capacity(2 * STEP_SAMPLES),
            frames,
            local,
            cycles,
            completed,
            input_done: false,
            finished: false,
        }
    }

    /// Fill `pcm` up to `capacity` samples. `true` once the traversal is over
    /// and everything it produced is in `pcm`.
    fn fill(
        &mut self,
        source: &mut dyn PcmSource,
        pcm: &mut Vec<f32>,
        capacity: usize,
        cancel: &AtomicBool,
    ) -> EngineResult<bool> {
        let ch = self.channels;
        let capacity = capacity / ch * ch;
        loop {
            if !self.out.is_empty() {
                let n = (capacity - pcm.len()).min(self.out.len()) / ch * ch;
                pcm.extend_from_slice(&self.out[..n]);
                self.out.drain(..n);
                if !self.out.is_empty() {
                    return Ok(false);
                }
            }
            if self.finished {
                return Ok(true);
            }
            if pcm.len() + ch > capacity {
                return Ok(false);
            }
            if cancel.load(Ordering::Acquire) {
                return Err(EngineError::from_detail(
                    ErrorCode::Cancelled,
                    "stream cancelled",
                ));
            }
            // The SRC writes only into reserved room (no allocation in the
            // loop once `out` reached its steady capacity).
            self.out.reserve(2 * STEP_SAMPLES);
            if self.sinc.is_preparing() {
                self.sinc.step(&[], false, &mut self.out, cancel)?;
                continue;
            }
            if self.head < self.native.len() {
                let step =
                    self.sinc
                        .step(&self.native[self.head..], false, &mut self.out, cancel)?;
                self.head += step.consumed;
                self.finished = step.progress == Progress::Finished;
                continue;
            }
            self.native.clear();
            self.head = 0;
            if self.input_done {
                let step = self.sinc.step(&[], true, &mut self.out, cancel)?;
                if step.progress == Progress::NeedInput {
                    // Input is over; an SRC still asking for more cannot finish.
                    return Err(invalid("SRC ended short of its declared input"));
                }
                self.finished = step.progress == Progress::Finished;
                continue;
            }
            if source.read(&mut self.native)? {
                self.local += (self.native.len() / ch) as u64;
                if self.frames.is_some_and(|n| self.local > n) {
                    return Err(invalid("source decoded past its presentation"));
                }
                continue;
            }
            if self.frames.is_some_and(|n| self.local != n) {
                return Err(invalid("source ended before its presentation"));
            }
            let completed = self.completed + 1;
            if self.cycles.is_none_or(|n| completed < n) {
                // The next pass continues the same input clock, so the SRC
                // sees one continuous signal across the loop point. Counted
                // only once the seek holds: an interrupted one is retried
                // from the same end-of-pass state.
                source.seek(0)?;
                self.local = 0;
            } else {
                self.input_done = true;
            }
            self.completed = completed;
        }
    }
}

struct Producer<'a> {
    tx: &'a mpsc::Sender<StreamMsg>,
    state: &'a Arc<StreamingState>,
    runtime: &'a tokio::runtime::Handle,
    source: Box<dyn PcmSource>,
    rate: u32,
    target: u32,
    channels: u32,
    clock: Option<StreamTimeline>,
    controls: Option<mpsc::Receiver<Command>>,
    publication: StreamPublication,
    pool: PcmPool,
    traversal: Traversal,
    /// SRC look-back in source frames and its phase period, both fixed by the
    /// rate pair.
    history: u64,
    period: u64,
    /// Samples per published chunk: a fixed duration, so the channel holds the
    /// same short time at every rate and a seek or error is not queued behind
    /// seconds of audio.
    chunk_samples: usize,
    /// The newest revision the consumer accepted; it may be ahead of
    /// `publication` between an accept and the traversal's start.
    accepted: StreamRevision,
    /// A read of the current traversal was abandoned mid-packet: the source
    /// must be put back at `traversal.local` before it is read again, unless
    /// a reposition replaces the traversal first.
    restore: bool,
}

enum Event<'t> {
    Permit(mpsc::Permit<'t, StreamMsg>),
    Command(Command),
    Stop,
}

enum Applied {
    /// The old traversal stays current (rejected, refused or not started).
    Unchanged,
    Repositioned,
    Stop,
}

fn run(
    open: Open,
    target: u32,
    tx: &mpsc::Sender<StreamMsg>,
    state: &Arc<StreamingState>,
    runtime: &tokio::runtime::Handle,
) -> Result<(), (StreamRevision, EngineError)> {
    let mut producer = start(open, target, tx, state, runtime)
        .map_err(|error| (StreamRevision::INITIAL, error))?;
    producer
        .produce()
        .map_err(|error| (producer.accepted, error))
}

fn start<'a>(
    open: Open,
    target: u32,
    tx: &'a mpsc::Sender<StreamMsg>,
    state: &'a Arc<StreamingState>,
    runtime: &'a tokio::runtime::Handle,
) -> EngineResult<Producer<'a>> {
    let source = open(state)?;
    let (rate, channels) = (source.rate(), source.channels());
    let clock = source
        .frames()
        .map(|frames| StreamTimeline::new(frames, rate, target, channels))
        .transpose()?;
    let mut publication = StreamPublication::default();
    let mut controls = None;
    if let Some(clock) = clock {
        state.bind_timeline(clock)?;
        state.bind_presentation(StreamPresentation::single(clock))?;
        publication.bind_presentation(StreamPresentation::single(clock), 0)?;
        if state.accepts_controls() {
            let (sender, receiver) = crate::stream_control::channel();
            state.install_controls(sender)?;
            controls = Some(receiver);
        }
    }
    let mut sinc = Sinc::new_at(rate, target, channels, source.frames(), 0, 0)?;
    let mut scratch = Vec::with_capacity(2 * STEP_SAMPLES);
    while sinc.is_preparing() {
        sinc.step(&[], false, &mut scratch, &state.cancelled)?;
    }
    let history = sinc.quiet_source_frames()?;
    let period = sinc.grid_period();
    let traversal = Traversal::new(sinc, channels, source.frames(), 0, Some(1));
    Ok(Producer {
        tx,
        state,
        runtime,
        source,
        rate,
        target,
        channels,
        clock,
        controls,
        publication,
        pool: PcmPool::bounded(),
        traversal,
        history,
        period,
        chunk_samples: chunk_samples(target, channels),
        accepted: StreamRevision::INITIAL,
        restore: false,
    })
}

impl Producer<'_> {
    fn produce(&mut self) -> EngineResult<()> {
        loop {
            if self.state.is_cancelled() {
                return Ok(());
            }
            let Some(mut pcm) = self.acquire() else {
                return Ok(());
            };
            let capacity = pcm.buffer_mut().capacity().min(self.chunk_samples);
            if std::mem::take(&mut self.restore) {
                self.source.seek(self.traversal.local)?;
            }
            self.state.set_read_preemption(true);
            let filled = self.traversal.fill(
                self.source.as_mut(),
                pcm.buffer_mut(),
                capacity,
                &self.state.cancelled,
            );
            self.state.set_read_preemption(false);
            let done = match filled {
                Ok(done) => done,
                // A control arrived while a read waited on the network: serve
                // it first. The reader stopped mid-packet, so it goes back to
                // where this traversal stands (the SRC state is intact) before
                // reading on, unless the control repositions it anyway (a
                // restore can cost a seek index).
                Err(error) if error.code == ErrorCode::Cancelled && !self.state.is_cancelled() => {
                    self.restore = true;
                    if pcm.buffer_mut().is_empty() {
                        drop(pcm);
                        match self.serve_control()? {
                            Applied::Stop => return Ok(()),
                            Applied::Repositioned | Applied::Unchanged => continue,
                        }
                    }
                    false
                }
                Err(error) => return Err(error),
            };
            let pending = self
                .publication
                .prepare_chunk(pcm, self.target, self.channels)?;
            match self.publish(pending)? {
                Applied::Stop => return Ok(()),
                Applied::Repositioned => continue,
                Applied::Unchanged => {}
            }
            if done {
                self.publication.validate_finish(true, 0)?;
                match self.finish()? {
                    Applied::Repositioned => continue,
                    Applied::Stop | Applied::Unchanged => return Ok(()),
                }
            }
        }
    }

    fn acquire(&mut self) -> Option<PcmChunk> {
        let (state, pool) = (self.state, &mut self.pool);
        self.runtime.block_on(async {
            tokio::select! {
                biased;
                _ = state.cancelled() => None,
                pcm = pool.acquire() => Some(pcm),
            }
        })
    }

    /// Wait for channel room, serving controls and cancellation meanwhile.
    fn next_event<'t>(
        runtime: &tokio::runtime::Handle,
        tx: &'t mpsc::Sender<StreamMsg>,
        state: &StreamingState,
        controls: Option<&mut mpsc::Receiver<Command>>,
    ) -> Event<'t> {
        runtime.block_on(async {
            let command = async {
                match controls {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                _ = state.cancelled() => Event::Stop,
                Some(command) = command => Event::Command(command),
                permit = tx.reserve() => match permit {
                    Ok(permit) => Event::Permit(permit),
                    Err(_) => Event::Stop,
                },
            }
        })
    }

    fn publish(&mut self, mut pending: PendingPublication) -> EngineResult<Applied> {
        while self.publication.has_pending(&pending) {
            #[cfg(test)]
            self.state
                .publication_waiting
                .store(self.tx.capacity() == 0, Ordering::Release);
            match Self::next_event(self.runtime, self.tx, self.state, self.controls.as_mut()) {
                Event::Permit(permit) => self.publication.publish_next(&mut pending, permit),
                Event::Command(command) => match self.apply(command)? {
                    Applied::Unchanged => {}
                    other => return Ok(other),
                },
                Event::Stop => return Ok(Applied::Stop),
            }
        }
        Ok(Applied::Unchanged)
    }

    /// Publish `Done`, then serve controls while the consumer retains the source.
    fn finish(&mut self) -> EngineResult<Applied> {
        loop {
            match Self::next_event(self.runtime, self.tx, self.state, self.controls.as_mut()) {
                Event::Permit(permit) => {
                    self.publication.finish_with(permit, self.state);
                    break;
                }
                Event::Command(command) => match self.apply(command)? {
                    Applied::Unchanged => {}
                    other => return Ok(other),
                },
                Event::Stop => return Ok(Applied::Stop),
            }
        }
        if !self.state.retains_source() || self.controls.is_none() {
            return Ok(Applied::Stop);
        }
        loop {
            match self.serve_control()? {
                Applied::Unchanged => {}
                other => return Ok(other),
            }
        }
    }

    /// Wait for the next control (or cancellation) and apply it.
    fn serve_control(&mut self) -> EngineResult<Applied> {
        let (state, controls) = (self.state, &mut self.controls);
        let Some(controls) = controls.as_mut() else {
            return Ok(Applied::Stop);
        };
        let command = self.runtime.block_on(async {
            tokio::select! {
                biased;
                _ = state.cancelled() => None,
                command = controls.recv() => command,
            }
        });
        match command {
            Some(command) => self.apply(command),
            None => Ok(Applied::Stop),
        }
    }

    /// The consumer's two-phase reposition: prepare the new traversal, offer
    /// it, and switch only once the consumer accepts it.
    fn apply(&mut self, command: Command) -> EngineResult<Applied> {
        let Command {
            target,
            cutover,
            deadline,
            reply,
        } = command;
        if reply.is_closed() {
            return Ok(Applied::Unchanged);
        }
        if Instant::now() >= deadline {
            let _ = reply.send(Reply::NotStarted(ErrorCode::Timeout));
            return Ok(Applied::Unchanged);
        }
        let Some(clock) = self.clock else {
            let _ = reply.send(Reply::NotStarted(ErrorCode::InvalidArgument));
            return Ok(Applied::Unchanged);
        };
        let traversal = match cutover
            .map(|request| {
                if request
                    .seek_target
                    .is_some_and(|n| i64::try_from(n).ok() != Some(target))
                {
                    return Err(invalid("source seek target changes its traversal"));
                }
                request.traversal(clock)
            })
            .transpose()
        {
            Ok(traversal) => traversal,
            Err(_) => {
                let _ = reply.send(Reply::Rejected(ErrorCode::InvalidArgument));
                return Ok(Applied::Unchanged);
            }
        };
        let plan = match self.plan(clock, target, cutover) {
            Ok(plan) => plan,
            Err(_) => {
                let _ = reply.send(Reply::Rejected(ErrorCode::InvalidArgument));
                return Ok(Applied::Unchanged);
            }
        };
        let revision = self.publication.revision().checked_next()?;
        self.state.set_read_deadline(Some(deadline));
        let prepared = self.prepare(&plan);
        self.state.set_read_deadline(None);
        let next = match prepared {
            Ok(next) => next,
            Err(error) if error.code == ErrorCode::Cancelled && self.state.is_cancelled() => {
                return Ok(Applied::Stop);
            }
            // A read the deadline interrupted: the consumer fails the stream
            // at that deadline, so this is terminal, not a refusal.
            Err(error) if error.code == ErrorCode::Cancelled => {
                return Err(EngineError::from_detail(
                    ErrorCode::Timeout,
                    "source control deadline passed while reading its media",
                ));
            }
            Err(error) => {
                // The old traversal stays current: the source goes back where
                // it was reading (everything before is already decoded) before
                // its next read.
                self.restore = true;
                let _ = reply.send(Reply::Rejected(error.code));
                return Ok(Applied::Unchanged);
            }
        };
        if Instant::now() >= deadline {
            // The consumer gives up on the request at its deadline and fails
            // the stream; preparing longer must not renew it.
            return Err(EngineError::from_detail(
                ErrorCode::Timeout,
                "source control deadline passed while preparing",
            ));
        }
        let (accept, receipt) = oneshot::channel();
        let prepared = Prepared {
            revision,
            target,
            traversal,
            source: self.state.clone(),
            deadline,
            accept,
        };
        if prepared.mapped_output()? != plan.start {
            return Err(invalid("source seek changed its output clock"));
        }
        if reply.send(Reply::Prepared(prepared)).is_err() {
            self.state.cancel();
            return Ok(Applied::Stop);
        }
        let state = self.state;
        let accepted = self.runtime.block_on(async {
            tokio::select! {
                biased;
                _ = state.cancelled() => false,
                accepted = tokio::time::timeout_at(deadline, receipt) => matches!(accepted, Ok(Ok(()))),
            }
        });
        // An acceptance is authoritative: the consumer checked the deadline
        // before sending it.
        if !accepted {
            self.state.cancel();
            return Ok(Applied::Stop);
        }
        self.accepted = revision;
        self.publication.begin(revision, self.state)?;
        let presentation = traversal
            .map(StreamTraversal::presentation)
            .unwrap_or(plan.presentation);
        self.publication
            .bind_presentation(presentation, plan.start)?;
        self.traversal = next;
        self.restore = false;
        Ok(Applied::Repositioned)
    }

    /// Where a reposition starts decoding and publishing (packet_control's
    /// window, which the consumer's clock arithmetic is written against).
    fn plan(
        &self,
        clock: StreamTimeline,
        target: i64,
        cutover: Option<LoopRequest>,
    ) -> EngineResult<Plan> {
        let frames = clock.input_frames();
        let (origin, start, cycles, presentation) = if let Some(request) =
            cutover.filter(|r| r.seek_target.is_none())
        {
            let plan =
                StreamCutover::new(clock, request.output_start, request.enabled, self.history)?;
            (
                plan.input_origin(),
                plan.output_start(),
                plan.cycles(),
                plan.presentation(),
            )
        } else {
            let frame = u64::try_from(target).map_err(|_| invalid("negative seek target"))?;
            let position = clock.position(frame)?;
            let start = position.frame + u64::from(position.phase != 0);
            if cutover.is_some_and(|r| r.seek_target != Some(frame) || r.output_start != start) {
                return Err(invalid("seek request changes its output mapping"));
            }
            let enabled = cutover.is_some_and(|r| r.enabled);
            let presentation = if enabled {
                StreamPresentation::repeated(clock, None)?
            } else {
                StreamPresentation::single(clock)
            };
            (
                frame.saturating_sub(self.history),
                start,
                (!enabled).then_some(1),
                presentation,
            )
        };
        Ok(Plan {
            origin: origin / self.period * self.period,
            start,
            end: match cycles {
                Some(n) => Some(
                    n.checked_mul(frames)
                        .ok_or_else(|| invalid("loop extent overflow"))?,
                ),
                None => None,
            },
            cycles,
            presentation,
            frames,
        })
    }

    fn prepare(&mut self, plan: &Plan) -> EngineResult<Traversal> {
        let sinc = Sinc::new_at(
            self.rate,
            self.target,
            self.channels,
            plan.end,
            plan.origin,
            plan.start,
        )?;
        let mut next = Traversal::new(
            sinc,
            self.channels,
            Some(plan.frames),
            plan.origin,
            plan.cycles,
        );
        if plan.presentation.output_end() == Some(plan.start) {
            // Nothing to play from here (a seek to the very end): the traversal
            // is over before any input, and the SRC is never driven.
            next.input_done = true;
            next.finished = true;
        } else {
            self.source.seek(plan.origin % plan.frames)?;
        }
        Ok(next)
    }
}

/// About 50 ms of output, between 64 frames and one SRC step.
fn chunk_samples(rate: u32, channels: u32) -> usize {
    let ch = channels as usize;
    let per_50ms = rate as usize * ch / 20;
    let samples = per_50ms.clamp(64 * ch, STEP_SAMPLES.max(64 * ch));
    samples / ch * ch
}

struct Plan {
    origin: u64,
    start: u64,
    end: Option<u64>,
    cycles: Option<u64>,
    presentation: StreamPresentation,
    frames: u64,
}

#[cfg(test)]
#[path = "stream_producer_tests.rs"]
mod tests;
