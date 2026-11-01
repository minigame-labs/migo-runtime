//! Exercise actual accepted input, original pending limits and CPU destruction.
use super::super::context_retirement_tests::{Dropped, block, wait, witness};
use super::super::tests::wav;
use super::*;
use std::sync::atomic::AtomicUsize;
const BYTES: usize = 4 << 20;
fn encoded() -> Vec<u8> {
    let mut data = Vec::with_capacity(BYTES);
    data.extend(wav(8000, 96));
    data
}
enum Answer {
    Buffer(Option<tokio::sync::oneshot::Receiver<EngineResult<DecodedPcm>>>),
    Inner(Option<tokio::sync::oneshot::Receiver<EngineResult<InnerAudioInfo>>>),
}
impl Answer {
    fn cancel(&mut self) {
        match self {
            Self::Buffer(v) => {
                v.take();
            }
            Self::Inner(v) => {
                v.take();
            }
        }
    }
    fn cancelled(&mut self) -> bool {
        match self {
            Self::Inner(Some(r)) => r.try_recv().is_ok_and(|r| {
                r.is_err_and(|e| {
                    e.code == ErrorCode::Cancelled
                        && e.detail.as_deref() == Some("audio decode request cancelled")
                })
            }),
            Self::Buffer(None) | Self::Inner(None) => true,
            _ => false,
        }
    }
    fn closed(&mut self) -> bool {
        match self {
            Self::Inner(Some(r)) => matches!(
                r.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Closed)
            ),
            Self::Buffer(Some(r)) => matches!(
                r.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Closed)
            ),
            _ => true,
        }
    }
}
fn async_sentinel() {
    let (tx, rx) = std::sync::mpsc::channel();
    crate::streaming::get_stream_runtime().spawn(async move {
        let _ = tx.send(());
    });
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
}
pub(super) fn waiting_custody() {
    for mode in [
        "reply",
        "source",
        "close",
        "drop",
        "slotclosed",
        "inflight",
        "unpolled",
    ] {
        case(mode);
    }
    successful_transfer();
}
fn case(mode: &str) {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let gate = block(&cpu);
    let (tx, results) = decode_queue(DECODE_JOB_QUEUE_CAPACITY, MAX_DECODE_RESULT_QUEUED_BYTES);
    let slots = if mode == "inflight" {
        DECODE_JOB_QUEUE_CAPACITY
    } else {
        0
    };
    let budget = Arc::new(DecodeInFlightUsage::new(1, 1));
    let held = if mode == "inflight" {
        Some(budget.try_reserve().unwrap())
    } else {
        None
    };
    let mut pool =
        DecodePool::new_with_in_flight(slots, tx, budget.clone(), ThreadWakeup::new(), cpu.clone());
    let pending = pool.pending.clone();
    let local = pool.slots.clone();
    let destroyed = Arc::new(Dropped::default());
    pool.retirement.hook = Some(witness(&destroyed));
    let queued = pool.retirement.queued.clone();
    let mut answers = Vec::new();
    let mut weak = Vec::new();
    let mut sources = Vec::new();
    let mut unpolled = Vec::new();
    for id in 0..DECODE_JOB_QUEUE_CAPACITY {
        let job = if id % 2 == 0 {
            let data = Arc::new(encoded());
            weak.push(Arc::downgrade(&data));
            let (resp, reply) = tokio::sync::oneshot::channel();
            answers.push(Answer::Buffer(Some(reply)));
            DecodeJob::AudioBuffer {
                ctx_id: id as u32,
                target_rate: 12000,
                data,
                resp,
            }
        } else {
            let source = Arc::new(crate::inner_audio::InnerAudioSharedState::new());
            let load = source.begin_source(7).unwrap();
            sources.push(source);
            let (resp, reply) = tokio::sync::oneshot::channel();
            answers.push(Answer::Inner(Some(reply)));
            DecodeJob::InnerAudio {
                id: id as u32,
                load,
                target_rate: 16000,
                data: encoded(),
                resp,
            }
        };
        if mode == "unpolled" {
            let waiting = pending.try_reserve(job.queued_bytes()).ok().unwrap();
            let admission = Admission {
                slots: pool.slots.clone(),
                budget: pool.in_flight.clone(),
                step_hook: None,
            };
            let value = Pending::new(job, waiting, &admission, &cpu, &mut pool.retirement);
            unpolled.push(run(
                value,
                admission,
                cpu.clone(),
                pool.result_tx.clone(),
                pool.wakeup.clone(),
            ));
        } else {
            assert!(pool.try_submit(job).is_none());
        }
    }
    assert!(
        pending.items.load(Ordering::Acquire) == DECODE_JOB_QUEUE_CAPACITY
            && pending.bytes.load(Ordering::Acquire) == MAX_DECODE_JOB_QUEUED_BYTES,
        "waiting pending credit returned before CPU destruction"
    );
    let before = migo_alloc_probe::thread_counts();
    match mode {
        "reply" => {
            for a in &mut answers {
                a.cancel();
            }
        }
        "source" => {
            for (i, a) in answers.iter_mut().enumerate() {
                if i % 2 == 0 {
                    a.cancel();
                }
            }
            for s in &sources {
                let _ = s.begin_source(8).unwrap();
            }
        }
        "close" => host.close(),
        "drop" => drop(pool),
        "slotclosed" => local.close(),
        "unpolled" => {
            host.close();
            drop(unpolled);
            drop(pool);
        }
        _ => {}
    }
    let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
    wait(|| queued.load(Ordering::Acquire) == DECODE_JOB_QUEUE_CAPACITY);
    async_sentinel();
    let credit = pending.items.load(Ordering::Acquire) == DECODE_JOB_QUEUE_CAPACITY
        && pending.bytes.load(Ordering::Acquire) == MAX_DECODE_JOB_QUEUED_BYTES
        && if matches!(mode, "drop" | "unpolled") {
            matches!(
                pending.try_reserve(0),
                Err(DecodeQueueReserveError::Disconnected)
            )
        } else {
            matches!(pending.try_reserve(0), Err(DecodeQueueReserveError::Full))
        };
    let alive = weak.iter().all(|w| w.upgrade().is_some());
    let cpu_only =
        destroyed.count.load(Ordering::Acquire) == 0 && !destroyed.wrong.load(Ordering::Acquire);
    let empty = results.try_recv().is_err();
    let reply = match mode {
        "source" => answers.iter_mut().all(Answer::cancelled),
        "drop" | "unpolled" => answers.iter_mut().all(Answer::closed),
        _ => true,
    };
    let failed_slots = mode != "inflight" || local.available_permits() == 0;
    gate.open();
    assert!(
        freed < (BYTES / 2) as u64 && alive && cpu_only,
        "waiting encoded input escaped CPU retirement"
    );
    assert!(
        credit,
        "{mode}: waiting pending credit returned before CPU destruction"
    );
    assert!(
        reply,
        "waiting cancellation or abort response waited for CPU cleanup"
    );
    assert!(
        empty,
        "waiting failure published before actual input retirement"
    );
    assert!(
        failed_slots,
        "in-flight refusal returned local slot before input retirement"
    );
    wait(|| destroyed.count.load(Ordering::Acquire) == DECODE_JOB_QUEUE_CAPACITY);
    assert!(
        !destroyed.wrong.load(Ordering::Acquire),
        "waiting encoded input escaped CPU retirement"
    );
    wait(|| {
        pending.items.load(Ordering::Acquire) == 0
            && pending.bytes.load(Ordering::Acquire) == 0
            && weak.iter().all(|w| w.upgrade().is_none())
    });
    if matches!(mode, "close" | "slotclosed" | "inflight") {
        for _ in 0..DECODE_JOB_QUEUE_CAPACITY {
            let result = results.recv_timeout(Duration::from_secs(15)).unwrap();
            let (id, error) = match result {
                DecodeResult::AudioBuffer { ctx_id, result, .. } => (ctx_id, result.err()),
                DecodeResult::InnerAudio { id, result, .. } => (id, result.err()),
            };
            assert!(
                id < DECODE_JOB_QUEUE_CAPACITY as u32,
                "waiting failure changed result identity"
            );
            let code = if mode == "inflight" {
                ErrorCode::InputSaturated
            } else {
                ErrorCode::Disconnected
            };
            assert!(
                error.is_some_and(|e| e.code == code
                    && (mode != "inflight"
                        || e.detail.as_deref()
                            == Some("audio decode in-flight memory budget exceeded"))),
                "waiting failure changed original typed result"
            );
        }
    } else {
        assert!(
            results.try_recv().is_err(),
            "cancelled waiting decode published a result"
        );
    }
    assert_eq!(
        local.available_permits(),
        slots,
        "waiting retirement leaked a local slot"
    );
    drop(held);
    assert_eq!(budget.bytes.load(Ordering::Acquire), 0);
    let peer = migo_io::scheduler::IoScheduler::new(1);
    assert_eq!(
        crate::streaming::get_stream_runtime()
            .block_on(peer.audio_cpu().run(|| 42))
            .unwrap(),
        42
    );
}
fn successful_transfer() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let (tx, results) = decode_queue(8, MAX_DECODE_RESULT_QUEUED_BYTES);
    let mut pool = DecodePool::new_with_in_flight(
        1,
        tx,
        Arc::new(DecodeInFlightUsage::new(1, 1)),
        ThreadWakeup::new(),
        host.audio_cpu(),
    );
    let seen = Arc::new(AtomicUsize::new(0));
    let observed = seen.clone();
    pool.step_hook = Some(Arc::new(move |_| {
        observed.fetch_add(1, Ordering::Release);
    }));
    let destroyed = Arc::new(Dropped::default());
    pool.retirement.hook = Some(witness(&destroyed));
    for inner in [false, true] {
        let (a, _ar) = tokio::sync::oneshot::channel();
        let (b, _br) = tokio::sync::oneshot::channel();
        let target = if inner { 16000 } else { 12000 };
        let source = Arc::new(crate::inner_audio::InnerAudioSharedState::new());
        let job = if inner {
            DecodeJob::InnerAudio {
                id: 7,
                load: source.begin_source(7).unwrap(),
                target_rate: target,
                data: encoded(),
                resp: b,
            }
        } else {
            DecodeJob::AudioBuffer {
                ctx_id: 7,
                target_rate: target,
                data: Arc::new(encoded()),
                resp: a,
            }
        };
        assert!(pool.try_submit(job).is_none());
        let audio = match results.recv_timeout(Duration::from_secs(15)).unwrap() {
            DecodeResult::AudioBuffer { ctx_id, result, .. } => {
                assert!(!inner && ctx_id == 7);
                result.unwrap()
            }
            DecodeResult::InnerAudio { id, result, .. } => {
                assert!(inner && id == 7);
                result.unwrap()
            }
        };
        let expected = crate::decoder::decode_for_rate(&wav(8000, 96), target).unwrap();
        assert!(
            audio.sample_rate == expected.sample_rate
                && audio.channels == expected.channels
                && audio.samples.len() == expected.samples.len()
                && audio
                    .samples
                    .iter()
                    .zip(&expected.samples)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
            "waiting handoff changed target or complete PCM"
        );
        assert!(
            pool.pending.items.load(Ordering::Acquire) == 0
                && pool.pending.bytes.load(Ordering::Acquire) == 0
                && pool.in_flight.bytes.load(Ordering::Acquire) == 0
                && pool.slots.available_permits() == 1,
            "successful waiting handoff published before original credit returned"
        );
    }
    assert!(seen.load(Ordering::Acquire) > 0);
    assert_eq!(
        pool.retirement.queued.load(Ordering::Acquire),
        0,
        "successful decode re-entered waiting retirement"
    );
    assert_eq!(destroyed.count.load(Ordering::Acquire), 0);
}
