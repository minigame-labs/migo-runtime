//! Accepted result transport owns the same byte/item ticket through refusal or
//! receiver teardown; raw successful response extraction keeps its old boundary.
use super::context_retirement_tests::{Dropped, block, wait, witness};
use super::*;
use std::sync::atomic::Ordering;
const BYTES: usize = 4 << 20;
fn host() -> (
    migo_io::scheduler::IoScheduler,
    migo_io::audio_cpu::AudioCpu,
) {
    let h = migo_io::scheduler::IoScheduler::isolated_for_admission_probe(931, 1, u64::MAX);
    let c = h.audio_cpu();
    (h, c)
}
fn sample(i: usize) -> f32 {
    if i == 0 {
        -0.0
    } else {
        (i % 31) as f32 / 32.0 - 0.5
    }
}
fn pcm() -> DecodedAudio {
    let mut samples = Vec::with_capacity(BYTES / 4);
    samples.extend((0..128).map(sample));
    DecodedAudio {
        samples,
        sample_rate: 48000,
        channels: 2,
    }
}
fn channel(
    capacity: usize,
) -> (
    DecodeQueueSender<DecodeResult>,
    DecodeQueueReceiver<DecodeResult>,
    Arc<Dropped>,
    migo_io::scheduler::IoScheduler,
    migo_io::audio_cpu::AudioCpu,
) {
    let (owner, cpu) = host();
    let (mut tx, rx) = result_custody::channel(capacity, BYTES * capacity, cpu.clone());
    let seen = Arc::new(Dropped::default());
    if let Some(retired) = tx.retirement.as_mut() {
        retired.hook = Some(witness(&seen));
    }
    (tx, rx, seen, owner, cpu)
}
fn buffer(
    data: EngineResult<DecodedAudio>,
) -> (
    DecodeResult,
    tokio::sync::oneshot::Receiver<EngineResult<DecodedPcm>>,
) {
    let (resp, rx) = tokio::sync::oneshot::channel();
    (
        DecodeResult::AudioBuffer {
            ctx_id: 1,
            result: data,
            resp,
        },
        rx,
    )
}
fn publish(
    tx: &DecodeQueueSender<DecodeResult>,
    data: DecodedAudio,
) -> tokio::sync::oneshot::Receiver<EngineResult<DecodedPcm>> {
    let (value, rx) = buffer(Ok(data));
    publish_decode_result(tx, value);
    rx
}
fn counters(tx: &DecodeQueueSender<DecodeResult>) -> (usize, usize) {
    (
        tx.usage.items.load(Ordering::Acquire),
        tx.usage.bytes.load(Ordering::Acquire),
    )
}
fn sentinel() {
    let (send, receive) = std::sync::mpsc::channel();
    crate::streaming::get_stream_runtime().spawn(async move {
        let _ = send.send(());
    });
    receive.recv_timeout(Duration::from_secs(5)).unwrap();
}
fn finish(tx: &DecodeQueueSender<DecodeResult>, seen: &Arc<Dropped>, count: usize) {
    wait(|| seen.count.load(Ordering::Acquire) == count && counters(tx) == (0, 0));
    assert!(
        !seen.wrong.load(Ordering::Acquire),
        "accepted result destructor escaped CPU workers"
    );
}
pub(super) fn custody() {
    for closed in [false, true] {
        for kind in ["buffer", "inner", "error"] {
            queue_exit(kind, closed);
        }
        for mode in ["closed", "context", "zero-channels", "partial", "send-race"] {
            integrate_refusal(mode, closed);
        }
        successful(closed);
        late_publication_refusal(closed);
    }
    physical_bound();
    upstream_error();
    legacy();
}
fn queue_exit(kind: &str, closed: bool) {
    let (tx, rx, seen, owner, cpu) = channel(2);
    let peer = owner.peer_for_admission_probe(932);
    let mut responses = Vec::new();
    let mut inner_responses = Vec::new();
    for id in 0..2 {
        let value = if kind == "inner" {
            let state = Arc::new(crate::inner_audio::InnerAudioSharedState::new());
            let load = state.begin_source(1).unwrap();
            let (resp, answer) = tokio::sync::oneshot::channel();
            inner_responses.push(answer);
            DecodeResult::InnerAudio {
                id,
                load,
                result: Ok(pcm()),
                resp,
            }
        } else {
            let (value, answer) = buffer(if kind == "error" {
                Err(EngineError::new(ErrorCode::IoError))
            } else {
                Ok(pcm())
            });
            responses.push(answer);
            value
        };
        publish_decode_result(&tx, value);
    }
    let charged = if kind == "error" { 0 } else { 2 * BYTES };
    assert_eq!(counters(&tx), (2, charged));
    let gate = block(&cpu);
    if closed {
        owner.close();
    }
    let before = migo_alloc_probe::thread_counts();
    drop(rx);
    sentinel();
    let prompt = responses.iter_mut().all(|r| {
        matches!(
            r.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        )
    }) && inner_responses.iter_mut().all(|r| {
        matches!(
            r.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        )
    });
    let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
    let credit = counters(&tx);
    let held = seen.count.load(Ordering::Acquire) == 0;
    let disconnected = matches!(
        tx.usage.try_reserve(0),
        Err(DecodeQueueReserveError::Disconnected)
    );
    gate.open();
    assert!(
        freed < (BYTES / 2) as u64 && held,
        "accepted result destructor escaped CPU workers"
    );
    assert_eq!(
        credit,
        (2, charged),
        "accepted result credit returned before actual CPU disposal"
    );
    assert!(
        prompt && disconnected,
        "result receiver exit delayed response close or reopened admission"
    );
    finish(&tx, &seen, 2);
    assert_eq!(
        crate::streaming::get_stream_runtime()
            .block_on(peer.audio_cpu().run(|| 42))
            .unwrap(),
        42
    );
}
fn integrate_refusal(mode: &str, closed: bool) {
    let (tx, rx, seen, owner, cpu) = channel(1);
    let mut data = pcm();
    if mode == "zero-channels" {
        data.channels = 0;
    }
    if mode == "partial" {
        data.samples.push(0.125);
    }
    let mut response = Some(publish(&tx, data));
    let mut contexts = HashMap::new();
    if mode != "context" {
        contexts.insert(1, AudioContext::new(1, 48000, 2));
    }
    let entry = rx.try_recv_owned().unwrap();
    assert_eq!(counters(&tx), (1, BYTES));
    if mode == "closed" {
        drop(response.take());
    }
    if mode == "send-race" {
        let answer = response.take().unwrap();
        result_custody::BEFORE_SEND
            .with(|slot| *slot.borrow_mut() = Some(Box::new(move || drop(answer))));
    }
    let gate = block(&cpu);
    if closed {
        owner.close();
    }
    let before = migo_alloc_probe::thread_counts();
    result_custody::integrate(entry, &contexts, &mut HashMap::new());
    sentinel();
    let response_ok = if let Some(mut response) = response {
        match response.try_recv() {
            Ok(Err(error)) => {
                error.code
                    == if mode == "context" {
                        ErrorCode::NotFound
                    } else {
                        ErrorCode::Internal
                    }
            }
            _ => false,
        }
    } else {
        true
    };
    let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
    let credit = counters(&tx);
    let held = seen.count.load(Ordering::Acquire) == 0;
    gate.open();
    assert!(
        response_ok,
        "accepted decode refusal changed typed response"
    );
    assert!(
        freed < (BYTES / 2) as u64 && held,
        "accepted result destructor escaped CPU workers"
    );
    assert_eq!(
        credit,
        (1, BYTES),
        "accepted result credit returned before actual CPU disposal"
    );
    assert!(contexts.values().all(|c| c.buffer_channels(1).is_none()));
    finish(&tx, &seen, 1);
}
fn successful(closed: bool) {
    let (tx, rx, seen, owner, cpu) = channel(1);
    let data = pcm();
    let pointer = data.samples.as_ptr();
    let mut response = publish(&tx, data);
    let contexts = HashMap::from([(1, AudioContext::new(1, 48000, 2))]);
    let entry = rx.try_recv_owned().unwrap();
    let gate = block(&cpu);
    if closed {
        owner.close();
    }
    result_custody::integrate(entry, &contexts, &mut HashMap::new());
    let answer = response.try_recv().unwrap().unwrap();
    let shape = (answer.sample_rate, answer.channels, answer.frames);
    let same = answer.samples.as_ptr() == pointer
        && answer.samples.capacity() * 4 == BYTES
        && answer
            .samples
            .iter()
            .enumerate()
            .all(|(i, v)| v.to_bits() == sample(i).to_bits());
    let credit = counters(&tx);
    gate.open();
    assert!(
        shape == (48000, 2, 64) && same,
        "accepted decode success changed complete PCM,shape or identity"
    );
    assert_eq!(
        credit,
        (1, BYTES),
        "successful response released queue admission before registry handoff"
    );
    assert_eq!(
        seen.count.load(Ordering::Acquire),
        0,
        "successful decode destroyed delivered backing"
    );
    assert_eq!(contexts[&1].buffer_channels(1), None);
    drop(answer);
    finish(&tx, &seen, 1);
}
fn late_publication_refusal(closed: bool) {
    let (tx, rx, seen, owner, cpu) = channel(1);
    let (value, mut answer) = buffer(Ok(pcm()));
    let permit = tx.usage.try_reserve(BYTES).ok().unwrap();
    let entry = result_custody::entry(value, permit, tx.retirement.as_ref());
    drop(rx);
    let gate = block(&cpu);
    if closed {
        owner.close();
    }
    let before = migo_alloc_probe::thread_counts();
    let Err(std_mpsc::TrySendError::Disconnected(entry)) = tx.tx.try_send(entry) else {
        panic!("late fixture channel remained connected");
    };
    result_custody::reject(
        entry,
        ErrorCode::Disconnected,
        DECODE_QUEUE_DISCONNECTED_DETAIL,
    );
    sentinel();
    let response = answer.try_recv().unwrap().unwrap_err().code == ErrorCode::Disconnected;
    let held = seen.count.load(Ordering::Acquire) == 0;
    let credit = counters(&tx);
    let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
    gate.open();
    assert!(response, "accepted late publication changed typed refusal");
    assert!(
        held && freed < (BYTES / 2) as u64,
        "accepted result destructor escaped CPU workers"
    );
    assert_eq!(
        credit,
        (1, BYTES),
        "accepted result credit returned before actual CPU disposal"
    );
    finish(&tx, &seen, 1);
}
fn physical_bound() {
    let (tx, rx, seen, _owner, cpu) = channel(2);
    let contexts = HashMap::new();
    let mut answers = Vec::new();
    for _ in 0..2 {
        answers.push(publish(&tx, pcm()));
    }
    let gate = block(&cpu);
    for _ in 0..2 {
        result_custody::integrate(rx.try_recv_owned().unwrap(), &contexts, &mut HashMap::new());
    }
    let full = matches!(tx.usage.try_reserve(0), Err(DecodeQueueReserveError::Full));
    let credit = counters(&tx);
    gate.open();
    assert!(
        full && credit == (2, BYTES * 2),
        "accepted result retirement bypassed original queue item or byte bound"
    );
    finish(&tx, &seen, 2);
    assert!(tx.usage.try_reserve(BYTES).is_ok());
}
fn upstream_error() {
    let (tx, rx, _seen, _owner, _cpu) = channel(1);
    let original = EngineError::from_detail(ErrorCode::IoError, "original decoder failure");
    let (value, mut response) = buffer(Err(original));
    publish_decode_result(&tx, value);
    result_custody::integrate(
        rx.try_recv_owned().unwrap(),
        &HashMap::new(),
        &mut HashMap::new(),
    );
    let error = response.try_recv().unwrap().unwrap_err();
    assert!(
        error.code == ErrorCode::IoError
            && error.detail.as_deref() == Some("original decoder failure"),
        "accepted original decoder error replaced by context refusal"
    );
    assert_eq!(counters(&tx), (0, 0));
}
fn legacy() {
    let (tx, rx) = decode_queue(1, BYTES);
    let mut response = publish(&tx, pcm());
    let before = migo_alloc_probe::thread_counts();
    drop(rx);
    assert!(migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed >= BYTES as u64);
    assert_eq!(counters(&tx), (0, 0));
    assert!(matches!(
        response.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Closed)
    ));
}
