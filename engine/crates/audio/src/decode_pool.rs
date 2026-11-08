//! Whole-file work shares the streaming runtime and the host's CPU executor.
use super::*;
use crate::{decoder::whole::WholeDecodeJob, off_worker::OffWorker};
use migo_io::audio_cpu::AudioCpu;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

// Each logical codec/PCM/SRC step keeps its own byte/call budget. Amortize
// executor handoff without draining the file: the hard step cap applies even
// when the clock has not advanced. Elapsed time is checked between native calls.
const MAX_STEPS_PER_TURN: usize = 128;
const MAX_TURN_TIME: Duration = Duration::from_millis(1);
const REFILL_STEPS_PER_TURN: usize = 32;
const REFILL_GRACE: Duration = Duration::from_millis(2);

// A short refill can finish before the next background turn observes pending.
// Keep the smaller turn budget briefly between requests in the same burst.
// Initializing at admission avoids treating old executor activity as new work.
struct RefillWindow {
    generation: usize,
    recent: Option<Instant>,
}
impl RefillWindow {
    fn new(generation: usize) -> Self {
        Self {
            generation,
            recent: None,
        }
    }
    fn step_limit(&mut self, generation: usize, now: Instant) -> usize {
        if generation != self.generation {
            self.generation = generation;
            self.recent = Some(now);
        }
        if self
            .recent
            .is_some_and(|recent| now.duration_since(recent) < REFILL_GRACE)
        {
            REFILL_STEPS_PER_TURN
        } else {
            MAX_STEPS_PER_TURN
        }
    }
}

#[cfg(test)]
type StepHook = Arc<dyn Fn(&Arc<AtomicBool>) + Send + Sync>;

pub(super) struct DecodePool {
    pub(super) pending: Arc<DecodeQueueUsage>,
    pub(super) slots: Arc<Semaphore>,
    pub(super) in_flight: Arc<DecodeInFlightUsage>,
    cpu: AudioCpu,
    result_tx: DecodeQueueSender<DecodeResult>,
    wakeup: ThreadWakeup,
    tasks: tokio::task::JoinSet<()>,
    #[cfg(test)]
    step_hook: Option<StepHook>,
}
impl DecodePool {
    pub(super) fn new(
        concurrency: usize,
        result_tx: DecodeQueueSender<DecodeResult>,
        wakeup: ThreadWakeup,
        cpu: AudioCpu,
    ) -> Self {
        Self::new_with_in_flight(
            concurrency,
            result_tx,
            process_decode_in_flight_budget(),
            wakeup,
            cpu,
        )
    }
    pub(super) fn new_with_in_flight(
        concurrency: usize,
        result_tx: DecodeQueueSender<DecodeResult>,
        in_flight: Arc<DecodeInFlightUsage>,
        wakeup: ThreadWakeup,
        cpu: AudioCpu,
    ) -> Self {
        Self {
            pending: Arc::new(DecodeQueueUsage {
                max_items: DECODE_JOB_QUEUE_CAPACITY,
                max_bytes: MAX_DECODE_JOB_QUEUED_BYTES,
                closed: AtomicBool::new(false),
                items: std::sync::atomic::AtomicUsize::new(0),
                bytes: std::sync::atomic::AtomicUsize::new(0),
            }),
            slots: Arc::new(Semaphore::new(concurrency)),
            in_flight,
            cpu: cpu.background(),
            result_tx,
            wakeup,
            tasks: tokio::task::JoinSet::new(),
            #[cfg(test)]
            step_hook: None,
        }
    }
    #[cfg(test)]
    pub(super) fn submit(&mut self, job: DecodeJob) {
        if let Some((job, code, detail)) = self.try_submit(job) {
            reject_decode_job(job, code, detail);
        }
    }

    /// On synchronous refusal, hand the original owner back without dropping it.
    pub(super) fn try_submit(&mut self, job: DecodeJob) -> Option<DecodeRefusal> {
        while self.tasks.try_join_next().is_some() {}
        let waiting = match self.pending.try_reserve(job.queued_bytes()) {
            Ok(permit) => permit,
            Err(error) => {
                let (code, detail) = match error {
                    DecodeQueueReserveError::Full => {
                        (ErrorCode::InputSaturated, DECODE_JOB_QUEUE_FULL_DETAIL)
                    }
                    DecodeQueueReserveError::ByteLimit => {
                        (ErrorCode::InputSaturated, DECODE_JOB_BYTE_LIMIT_DETAIL)
                    }
                    DecodeQueueReserveError::Disconnected => {
                        (ErrorCode::Disconnected, DECODE_QUEUE_DISCONNECTED_DETAIL)
                    }
                };
                return Some((job, code, detail));
            }
        };
        let admission = Admission {
            slots: self.slots.clone(),
            budget: self.in_flight.clone(),
            #[cfg(test)]
            step_hook: self.step_hook.clone(),
        };
        // Prepare custody before spawning: even an unpolled future can be aborted.
        let pending = Pending::new(job, waiting, &admission, &self.cpu);
        self.tasks.spawn_on(
            run(
                pending,
                admission,
                self.cpu.clone(),
                self.result_tx.clone(),
                self.wakeup.clone(),
            ),
            crate::streaming::get_stream_runtime().handle(),
        );
        None
    }
}
impl Drop for DecodePool {
    fn drop(&mut self) {
        self.pending.closed.store(true, Ordering::Release);
        // JoinSet aborts only this owner's async tasks. A running CPU step keeps
        // its input, PCM and lease until it exits; other hosts keep their executor.
        self.tasks.abort_all();
    }
}

// Arc<Vec<u8>> does not implement AsRef<[u8]>. This adapter retains the original
// allocation for decodeAudioData and moves InnerAudio's exclusive allocation.
enum Input {
    Shared(Arc<Vec<u8>>),
    Owned(Vec<u8>),
}
impl AsRef<[u8]> for Input {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Shared(data) => data,
            Self::Owned(data) => data,
        }
    }
}
enum Reply {
    Buffer(AudioContextId, AudioResp<DecodedPcm>),
    Inner(
        InnerAudioId,
        crate::inner_audio::InnerAudioLoadToken,
        AudioResp<InnerAudioInfo>,
    ),
}
impl Reply {
    async fn cancelled(&mut self) {
        match self {
            Self::Buffer(_, resp) => resp.closed().await,
            Self::Inner(_, load, resp) => {
                let _ = load.watch().while_current(resp.closed()).await;
            }
        }
    }
    fn result(self, result: EngineResult<DecodedAudio>) -> DecodeResult {
        match self {
            Self::Buffer(ctx_id, resp) => DecodeResult::AudioBuffer {
                ctx_id,
                result,
                resp,
            },
            Self::Inner(id, load, resp) => DecodeResult::InnerAudio {
                id,
                load,
                result,
                resp,
            },
        }
    }
}

#[derive(Default)]
struct Retired {
    active: AtomicBool,
    done: AtomicBool,
    changed: Notify,
}
impl Retired {
    async fn wait(&self) {
        let notified = self.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if !self.done.load(Ordering::Acquire) {
            notified.await;
        }
    }
}
struct Retirement(Arc<Retired>);
impl Drop for Retirement {
    fn drop(&mut self) {
        self.0.done.store(true, Ordering::Release);
        self.0.changed.notify_waiters();
    }
}
// Declaration order matters: the final notification follows every resource.
struct Resources {
    waiting: Option<DecodeQueuePermit>,
    in_flight: Option<DecodeInFlightPermit>,
    slot: Option<OwnedSemaphorePermit>,
    _retirement: Retirement,
}
struct State {
    refill: RefillWindow,
    input: Option<Input>,
    job: Option<WholeDecodeJob<Input>>,
    resources: Option<Resources>,
    target: u32,
    cancel: Arc<AtomicBool>,
    #[cfg(test)]
    step_hook: Option<StepHook>,
    #[cfg(test)]
    last_turn_steps: usize,
}
struct Complete {
    audio: Option<DecodedAudio>,
    _resources: Resources,
}
impl State {
    fn turn(&mut self, cpu: &AudioCpu) -> EngineResult<Option<Complete>> {
        #[cfg(test)]
        if let Some(hook) = &self.step_hook {
            hook(&self.cancel);
        }
        self.turn_with_refill_budget(cpu, MAX_TURN_TIME, Instant::now())
    }

    fn turn_with_refill_budget(
        &mut self,
        cpu: &AudioCpu,
        max_elapsed: Duration,
        now: Instant,
    ) -> EngineResult<Option<Complete>> {
        let generation = cpu.refill_generation();
        let limit = self.refill.step_limit(generation, now);
        self.turn_with_budget(max_elapsed, limit, || {
            cpu.refill_pending() || cpu.refill_generation() != generation
        })
    }

    fn turn_with_budget(
        &mut self,
        max_elapsed: Duration,
        step_limit: usize,
        mut refill_queued: impl FnMut() -> bool,
    ) -> EngineResult<Option<Complete>> {
        let start = Instant::now();
        #[cfg(test)]
        {
            self.last_turn_steps = 0;
        }
        for _step in 0..step_limit.clamp(1, MAX_STEPS_PER_TURN) {
            let outcome = self.step()?;
            #[cfg(test)]
            {
                self.last_turn_steps = _step + 1;
            }
            if outcome.is_some() {
                return Ok(outcome);
            }
            if refill_queued() || start.elapsed() >= max_elapsed {
                break;
            }
        }
        Ok(None)
    }

    fn step(&mut self) -> EngineResult<Option<Complete>> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(EngineError::new(ErrorCode::Cancelled));
        }
        if let Some(input) = self.input.take() {
            self.job = Some(WholeDecodeJob::new(input, Some(self.target))?);
            return Ok(None);
        }
        if !self.job.as_mut().unwrap().step(&self.cancel)? {
            return Ok(None);
        }
        let audio = self.job.take().unwrap().finish()?;
        Ok(Some(Complete {
            audio: Some(audio),
            _resources: self.resources.take().unwrap(),
        }))
    }
}
struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
async fn decode(
    mut state: Box<State>,
    slots: Arc<Semaphore>,
    budget: Arc<DecodeInFlightUsage>,
    cpu: AudioCpu,
) -> EngineResult<Complete> {
    let slot = slots
        .acquire_owned()
        .await
        .map_err(|_| EngineError::new(ErrorCode::Disconnected))?;
    let resources = state.resources.as_mut().unwrap();
    resources.slot = Some(slot);
    resources.in_flight = Some(budget.try_reserve().ok_or_else(|| {
        EngineError::from_detail(
            ErrorCode::InputSaturated,
            "audio decode in-flight memory budget exceeded",
        )
    })?);
    // Failure above still owns the pending ticket; cleanup therefore cannot
    // exceed that original queue even when CPU retirement is blocked.
    resources
        ._retirement
        .0
        .active
        .store(true, Ordering::Release);
    drop(resources.waiting.take());
    let mut owner = OffWorker::from_box(state, cpu.clone());
    loop {
        let step_cpu = cpu.clone();
        let (returned, result) = owner.with(move |state| state.turn(&step_cpu)).await?;
        owner = returned;
        if let Some(complete) = result? {
            return Ok(complete);
        }
    }
}
struct Admission {
    slots: Arc<Semaphore>,
    budget: Arc<DecodeInFlightUsage>,
    #[cfg(test)]
    step_hook: Option<StepHook>,
}
struct Pending {
    state: Box<State>,
    reply: Reply,
    retired: Arc<Retired>,
    cancel: Arc<AtomicBool>,
}
impl Pending {
    fn new(
        job: DecodeJob,
        waiting: DecodeQueuePermit,
        _admission: &Admission,
        cpu: &AudioCpu,
    ) -> Self {
        let (input, target, reply) = match job {
            DecodeJob::AudioBuffer {
                ctx_id,
                target_rate,
                data,
                resp,
            } => (
                Input::Shared(data),
                target_rate,
                Reply::Buffer(ctx_id, resp),
            ),
            DecodeJob::InnerAudio {
                id,
                load,
                target_rate,
                data,
                resp,
            } => (
                Input::Owned(data),
                target_rate,
                Reply::Inner(id, load, resp),
            ),
        };
        let retired = Arc::new(Retired::default());
        let cancel = Arc::new(AtomicBool::new(false));
        let state = State {
            refill: RefillWindow::new(cpu.refill_generation()),
            input: Some(input),
            job: None,
            target,
            cancel: cancel.clone(),
            #[cfg(test)]
            step_hook: _admission.step_hook.clone(),
            #[cfg(test)]
            last_turn_steps: 0,
            resources: Some(Resources {
                waiting: Some(waiting),
                in_flight: None,
                slot: None,
                _retirement: Retirement(retired.clone()),
            }),
        };
        Self {
            state: Box::new(state),
            reply,
            retired,
            cancel,
        }
    }
}
async fn run(
    pending: Pending,
    admission: Admission,
    cpu: AudioCpu,
    result_tx: DecodeQueueSender<DecodeResult>,
    wakeup: ThreadWakeup,
) {
    let Pending {
        state,
        mut reply,
        retired,
        cancel,
    } = pending;
    let outcome = {
        let operation = decode(state, admission.slots, admission.budget, cpu.clone());
        tokio::pin!(operation);
        let _cancel = CancelOnDrop(cancel);
        tokio::select! {
            biased;
            _ = reply.cancelled() => Err(EngineError::new(ErrorCode::Cancelled)),
            _ = cpu.closed() => Err(EngineError::new(ErrorCode::Disconnected)),
            result = &mut operation => result,
        }
    };
    match outcome {
        Ok(mut complete) => {
            let result = reply.result(Ok(complete.audio.take().unwrap()));
            // The in-flight budget ends here; the result queue charges its PCM.
            drop(complete);
            publish_decode_result(&result_tx, result);
        }
        Err(error) => {
            let error = if error.code != ErrorCode::Cancelled && cpu.is_closed() {
                EngineError::new(ErrorCode::Disconnected)
            } else {
                error
            };
            // Host-close may win while native CPU work is still running. Its
            // captured state, not this async waiter, owns the memory allowance.
            if error.code == ErrorCode::Cancelled && !retired.active.load(Ordering::Acquire) {
                // Waiting cancellation answers promptly while original input
                // credit remains with the CPU cleanup, including after close.
                reject_decode_result(
                    reply.result(Err(error)),
                    ErrorCode::Cancelled,
                    "audio decode request cancelled",
                );
                retired.wait().await;
                wakeup.notify();
                return;
            }
            retired.wait().await;
            if error.code == ErrorCode::Cancelled {
                reject_decode_result(
                    reply.result(Err(error)),
                    ErrorCode::Cancelled,
                    "audio decode request cancelled",
                );
            } else {
                publish_decode_result(&result_tx, reply.result(Err(error)));
            }
        }
    }
    wakeup.notify();
}

#[cfg(test)]
#[path = "decode_pool_tests.rs"]
mod tests;
