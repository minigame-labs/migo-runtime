//! Refused encoded bytes keep their original transport admission through CPU drop.
use super::super::context_retirement_tests::{Dropped, block, wait, witness};
use super::super::tests::wav;
use super::*;
use shared::audio_channel::{
    AUDIO_COMMAND_CAPACITY, AudioCommandReserveError, MAX_AUDIO_COMMAND_QUEUED_BYTES,
};
use std::sync::atomic::Ordering;
const BYTES: usize = 4 << 20;
fn encoded(capacity: usize) -> Vec<u8> {
    let bytes = wav(8000, 96);
    let mut data = Vec::with_capacity(capacity);
    data.extend(bytes);
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
    fn error(&mut self, code: ErrorCode, detail: &str) -> bool {
        let error = match self {
            Self::Buffer(Some(r)) => r.try_recv().ok().and_then(Result::err),
            Self::Inner(Some(r)) => r.try_recv().ok().and_then(Result::err),
            _ => return true,
        };
        error.is_some_and(|e| e.code == code && e.detail.as_deref() == Some(detail))
    }
}
fn commands(
    bytes: usize,
    players: &mut HashMap<InnerAudioId, InnerAudioPlayer>,
) -> (
    Vec<AudioCmd>,
    Vec<Answer>,
    std::sync::Weak<Vec<u8>>,
    shared::audio_load::AudioLoadWatch,
    shared::audio_load::AudioLoadWatch,
) {
    let buffer = Arc::new(encoded(bytes));
    let weak = Arc::downgrade(&buffer);
    let old = players.get_mut(&2).unwrap().begin_load().unwrap().watch();
    let prepared = players.get_mut(&3).unwrap().begin_load_for(7).unwrap();
    let watch = prepared.watch();
    let (a, ar) = tokio::sync::oneshot::channel();
    let (b, br) = tokio::sync::oneshot::channel();
    let (c, cr) = tokio::sync::oneshot::channel();
    (
        vec![
            AudioCmd::DecodeAudioData {
                ctx_id: 1,
                data: buffer,
                resp: a,
            },
            AudioCmd::InnerAudioLoad {
                id: 2,
                data: encoded(bytes),
                resp: b,
            },
            AudioCmd::InnerAudioCompleteLoad {
                id: 3,
                load: prepared,
                data: encoded(bytes),
                resp: c,
            },
        ],
        vec![
            Answer::Buffer(Some(ar)),
            Answer::Inner(Some(br)),
            Answer::Inner(Some(cr)),
        ],
        weak,
        old,
        watch,
    )
}
fn players() -> HashMap<InnerAudioId, InnerAudioPlayer> {
    HashMap::from([
        (2, InnerAudioPlayer::new(2, 1)),
        (3, InnerAudioPlayer::new(3, 1)),
    ])
}
fn lazy(cpu: migo_io::audio_cpu::AudioCpu) -> (LazyDecodePool, DecodeQueueReceiver<DecodeResult>) {
    let (send, recv) = decode_queue(8, MAX_DECODE_RESULT_QUEUED_BYTES);
    (
        LazyDecodePool::new(send, 16000, ThreadWakeup::new(), cpu),
        recv,
    )
}
pub(crate) fn decode_refusals() {
    for closed in [false, true] {
        for mode in ["items", "bytes", "disconnected", "cancelled"] {
            let host = migo_io::scheduler::IoScheduler::new(1);
            let cpu = host.audio_cpu();
            let (mut pool, results) = lazy(cpu.clone());
            let mut players = players();
            let contexts = HashMap::from([(1, AudioContext::new(1, 12000, 1))]);
            let mut decode_fill = Vec::new();
            if mode != "cancelled" {
                pool.pool = Some(DecodePool::new(
                    0,
                    pool.result_tx.clone(),
                    pool.wakeup.clone(),
                    cpu.clone(),
                ));
                let p = pool.pool.as_ref().unwrap();
                match mode {
                    "items" => {
                        for _ in 0..DECODE_JOB_QUEUE_CAPACITY {
                            decode_fill.push(p.pending.try_reserve(0).ok().unwrap());
                        }
                    }
                    "bytes" => decode_fill.push(
                        p.pending
                            .try_reserve(MAX_DECODE_JOB_QUEUED_BYTES)
                            .ok()
                            .unwrap(),
                    ),
                    _ => p.pending.closed.store(true, Ordering::Release),
                }
            }
            let before_pending = pool.pool.as_ref().map(|p| {
                (
                    p.pending.items.load(Ordering::Acquire),
                    p.pending.bytes.load(Ordering::Acquire),
                )
            });
            let (commands, mut answers, weak, old, prepared) = commands(BYTES, &mut players);
            if mode == "cancelled" {
                for answer in &mut answers {
                    answer.cancel();
                }
            }
            let (tx, rx) = shared::audio_channel::channel();
            for cmd in commands {
                tx.try_send(cmd).unwrap();
            }
            let backlog = (0..2).map(|_| rx.try_recv_owned().unwrap()).collect();
            let mut fillers = vec![
                tx.try_reserve_data(MAX_AUDIO_COMMAND_QUEUED_BYTES - 3 * BYTES)
                    .unwrap(),
            ];
            for _ in 4..AUDIO_COMMAND_CAPACITY {
                fillers.push(tx.try_reserve_data(0).unwrap());
            }
            let mut inbox = crate::command_inbox::Inbox::new(rx, backlog, cpu.clone());
            let state = Arc::new(Dropped::default());
            inbox.observe(witness(&state));
            let gate = block(&cpu);
            if closed {
                host.close();
            }
            let before = migo_alloc_probe::thread_counts();
            let mut handled = 0;
            for _ in 0..3 {
                if forward(
                    inbox.next().unwrap(),
                    &contexts,
                    &mut players,
                    &mut pool,
                    &mut inbox,
                )
                .is_none()
                {
                    handled += 1;
                }
            }
            let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
            let (code, detail) = match mode {
                "items" => (ErrorCode::InputSaturated, DECODE_JOB_QUEUE_FULL_DETAIL),
                "bytes" => (ErrorCode::InputSaturated, DECODE_JOB_BYTE_LIMIT_DETAIL),
                "disconnected" => (ErrorCode::Disconnected, DECODE_QUEUE_DISCONNECTED_DETAIL),
                _ => (ErrorCode::Cancelled, "audio decode request cancelled"),
            };
            let replied = answers.iter_mut().all(|answer| answer.error(code, detail));
            let held = weak.upgrade().is_some();
            let item = matches!(tx.try_reserve_data(0), Err(AudioCommandReserveError::Full));
            drop(fillers.pop());
            let byte = matches!(
                tx.try_reserve_data(1),
                Err(AudioCommandReserveError::ByteLimit)
            );
            let mut extra = Vec::new();
            while let Ok(p) = tx.try_reserve_data(0) {
                extra.push(p);
            }
            let free = extra.len();
            drop(extra);
            let after_pending = pool.pool.as_ref().map(|p| {
                (
                    p.pending.items.load(Ordering::Acquire),
                    p.pending.bytes.load(Ordering::Acquire),
                )
            });
            let side_effect = !old.is_current() && prepared.is_current();
            let no_result = results.try_recv().is_err();
            gate.open();
            assert_eq!(handled, 3, "bulk decode refusal bypassed owned dispatch");
            assert!(
                freed < (BYTES / 2) as u64 && held,
                "refused decode input escaped CPU retirement"
            );
            assert!(
                replied,
                "decode refusal changed typed response or waited for CPU drop"
            );
            assert!(side_effect, "decode admission changed source generations");
            assert!(
                no_result && before_pending == after_pending,
                "refused decode mutated decoder admission or published a result"
            );
            assert!(
                mode != "cancelled" || !pool.is_started(),
                "cancelled request started the lazy decoder"
            );
            wait(|| state.count.load(Ordering::Acquire) == 3);
            assert!(
                !state.wrong.load(Ordering::Acquire),
                "refused decode input escaped CPU retirement"
            );
            assert!(
                item && byte && free == 1,
                "decode refusal returned transport credit before CPU drop"
            );
            wait(|| weak.upgrade().is_none());
            let returned = tx.try_reserve_data(3 * BYTES).unwrap();
            drop((returned, fillers, decode_fill, pool, inbox));
            drop(host);
        }
    }
    generation_refusal();
    accepted();
    compatibility();
}
fn generation_refusal() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let (mut pool, _) = lazy(cpu.clone());
    let mut players = players();
    players
        .get(&2)
        .unwrap()
        .shared
        .exhaust_source_generation_for_test();
    let (resp, mut answer) = tokio::sync::oneshot::channel();
    let (tx, rx) = shared::audio_channel::channel();
    tx.try_send(AudioCmd::InnerAudioLoad {
        id: 2,
        data: encoded(BYTES),
        resp,
    })
    .unwrap();
    let mut inbox = crate::command_inbox::Inbox::new(rx, vec![], cpu.clone());
    let state = Arc::new(Dropped::default());
    inbox.observe(witness(&state));
    let gate = block(&cpu);
    let before = migo_alloc_probe::thread_counts();
    let handled = forward(
        inbox.next().unwrap(),
        &HashMap::new(),
        &mut players,
        &mut pool,
        &mut inbox,
    )
    .is_none();
    let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
    let error = answer.try_recv().ok().and_then(Result::err);
    let held = matches!(
        tx.try_reserve_data(MAX_AUDIO_COMMAND_QUEUED_BYTES),
        Err(AudioCommandReserveError::ByteLimit)
    );
    gate.open();
    assert!(
        handled && freed < (BYTES / 2) as u64,
        "generation refusal lost original decode input"
    );
    assert!(
        error.is_some_and(|e| e.code == ErrorCode::InputSaturated
            && e.detail.as_deref() == Some("InnerAudio source generation exhausted"))
            && !pool.is_started(),
        "generation failure changed response or started decoder"
    );
    wait(|| state.count.load(Ordering::Acquire) == 1);
    assert!(
        held && !state.wrong.load(Ordering::Acquire),
        "generation refusal lost CPU ownership or input credit"
    );
}
fn accepted() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let (mut pool, results) = lazy(cpu.clone());
    pool.pool = Some(DecodePool::new_with_in_flight(
        0,
        pool.result_tx.clone(),
        Arc::new(DecodeInFlightUsage::new(3, 1)),
        pool.wakeup.clone(),
        cpu.clone(),
    ));
    let contexts = HashMap::from([(1, AudioContext::new(1, 12000, 1))]);
    let mut players = players();
    let (commands, mut answers, weak, old, prepared) = commands(BYTES, &mut players);
    let (tx, rx) = shared::audio_channel::channel();
    for cmd in commands {
        tx.try_send(cmd).unwrap();
    }
    let mut fillers = vec![
        tx.try_reserve_data(MAX_AUDIO_COMMAND_QUEUED_BYTES - 3 * BYTES)
            .unwrap(),
    ];
    for _ in 4..AUDIO_COMMAND_CAPACITY {
        fillers.push(tx.try_reserve_data(0).unwrap());
    }
    let mut inbox = crate::command_inbox::Inbox::new(rx, vec![], cpu.clone());
    let state = Arc::new(Dropped::default());
    inbox.observe(witness(&state));
    let gate = block(&cpu);
    let mut handled = 0;
    for _ in 0..3 {
        if forward(
            inbox.next().unwrap(),
            &contexts,
            &mut players,
            &mut pool,
            &mut inbox,
        )
        .is_none()
        {
            handled += 1;
        }
    }
    let returned = tx.try_reserve_data(3 * BYTES).is_ok();
    let mut extra = Vec::new();
    while let Ok(p) = tx.try_reserve_data(0) {
        extra.push(p);
    }
    let free = extra.len();
    drop(extra);
    let p = pool.pool.as_ref().unwrap();
    let pending = (
        p.pending.items.load(Ordering::Acquire),
        p.pending.bytes.load(Ordering::Acquire),
    );
    let owned = weak.upgrade().is_some();
    let queued = state.count.load(Ordering::Acquire);
    pool.sample_rate = 32000;
    pool.pool.as_ref().unwrap().slots.add_permits(1);
    gate.open();
    assert!(
        handled == 3 && returned && free == 3 && queued == 0,
        "accepted decode retained original transport credit"
    );
    assert!(
        owned && pending == (3, 3 * BYTES),
        "accepted decode lost original input or decoder allowance"
    );
    assert!(
        !old.is_current() && prepared.is_current(),
        "decode admission changed source generations"
    );
    let mut seen = Vec::new();
    for _ in 0..3 {
        let result = results.recv_timeout(Duration::from_secs(15)).unwrap();
        let id = match result {
            DecodeResult::AudioBuffer {
                ctx_id,
                result,
                resp,
            } => {
                let audio = result.unwrap();
                let id = ctx_id;
                let rate = 12000;
                let expected = decode_for_rate(&wav(8000, 96), rate).unwrap();
                assert!(
                    same(&audio, &expected),
                    "decode admission changed target rate or complete PCM"
                );
                integrate_audio_buffer_decode_result(&contexts, ctx_id, Ok(audio), resp);
                id
            }
            DecodeResult::InnerAudio {
                id,
                load,
                result,
                resp,
            } => {
                let audio = result.unwrap();
                let expected = decode_for_rate(&wav(8000, 96), 16000).unwrap();
                assert!(
                    same(&audio, &expected),
                    "decode admission changed target rate or complete PCM"
                );
                assert!(
                    players.get(&id).unwrap().shared.owns_load(&load),
                    "accepted decode lost source owner"
                );
                integrate_inner_audio_decode_result(&mut players, id, load, Ok(audio), resp);
                id
            }
        };
        seen.push(id);
    }
    seen.sort_unstable();
    assert_eq!(seen, [1, 2, 3], "decode admission changed result identity");
    assert!(
        match &mut answers[0] {
            Answer::Buffer(Some(r)) => r
                .try_recv()
                .is_ok_and(|r| r.is_ok_and(|r| r.sample_rate == 12000)),
            _ => false,
        },
        "accepted buffer response changed"
    );
    for answer in &mut answers[1..] {
        assert!(
            match answer {
                Answer::Inner(Some(r)) => r.try_recv().is_ok_and(|r| r.is_ok()),
                _ => false,
            },
            "accepted inner response changed"
        );
    }
    wait(|| {
        weak.upgrade().is_none()
            && pool
                .pool
                .as_ref()
                .unwrap()
                .pending
                .items
                .load(Ordering::Acquire)
                == 0
            && pool
                .pool
                .as_ref()
                .unwrap()
                .in_flight
                .bytes
                .load(Ordering::Acquire)
                == 0
    });
    fillers.clear();
}
fn same(a: &DecodedAudio, b: &DecodedAudio) -> bool {
    a.sample_rate == b.sample_rate
        && a.channels == b.channels
        && a.samples.len() == b.samples.len()
        && a.samples
            .iter()
            .zip(&b.samples)
            .all(|(a, b)| a.to_bits() == b.to_bits())
}
fn compatibility() {
    for (bytes, unmanaged) in [(4095, false), (4096, false), (BYTES, true)] {
        let host = migo_io::scheduler::IoScheduler::new(1);
        let cpu = host.audio_cpu();
        let (mut pool, _) = lazy(cpu.clone());
        pool.pool = Some(DecodePool::new(
            0,
            pool.result_tx.clone(),
            pool.wakeup.clone(),
            cpu.clone(),
        ));
        let mut held = Vec::new();
        for _ in 0..DECODE_JOB_QUEUE_CAPACITY {
            held.push(
                pool.pool
                    .as_ref()
                    .unwrap()
                    .pending
                    .try_reserve(0)
                    .ok()
                    .unwrap(),
            );
        }
        let contexts = HashMap::from([(1, AudioContext::new(1, 12000, 1))]);
        let mut players = players();
        let (commands, mut answers, weak, _, _) = commands(bytes, &mut players);
        let (tx, rx) = shared::audio_channel::channel();
        for cmd in commands {
            tx.try_send(cmd).unwrap();
        }
        let mut inbox = crate::command_inbox::Inbox::new(rx, vec![], cpu.clone());
        let state = Arc::new(Dropped::default());
        inbox.observe(witness(&state));
        let gate = block(&cpu);
        let mut handled = 0;
        for _ in 0..3 {
            let d = inbox.next().unwrap();
            let d = if unmanaged {
                let (cmd, p) = d.into_parts();
                drop(p);
                AudioCommandDelivery::unmanaged(cmd)
            } else {
                d
            };
            if let Some(d) = forward(d, &contexts, &mut players, &mut pool, &mut inbox) {
                let (cmd, p) = d.into_parts();
                match cmd {
                    AudioCmd::DecodeAudioData { ctx_id, data, resp } => {
                        pool.submit(DecodeJob::AudioBuffer {
                            ctx_id,
                            target_rate: 12000,
                            data,
                            resp,
                        })
                    }
                    AudioCmd::InnerAudioLoad { id, data, resp } => {
                        let load = players.get_mut(&id).unwrap().begin_load().unwrap();
                        pool.submit_inner_audio(id, load, data, resp);
                    }
                    AudioCmd::InnerAudioCompleteLoad {
                        id,
                        load,
                        data,
                        resp,
                    } => {
                        submit_prepared_inner_audio_load(&players, &mut pool, id, load, data, resp)
                    }
                    _ => unreachable!(),
                };
                drop(p);
            } else {
                handled += 1;
            }
        }
        let large = bytes >= 4096 && !unmanaged;
        let alive = weak.upgrade().is_some();
        let bytes_held = matches!(
            tx.try_reserve_data(MAX_AUDIO_COMMAND_QUEUED_BYTES),
            Err(AudioCommandReserveError::ByteLimit)
        );
        gate.open();
        assert_eq!(
            handled,
            if large { 3 } else { 0 },
            "decode refusal small or legacy dispatch changed"
        );
        assert!(
            alive == large && bytes_held == large,
            "small or legacy decode unnecessarily retained input"
        );
        assert!(
            answers
                .iter_mut()
                .all(|a| a.error(ErrorCode::InputSaturated, DECODE_JOB_QUEUE_FULL_DETAIL)),
            "legacy decode refusal changed typed response"
        );
        if large {
            wait(|| state.count.load(Ordering::Acquire) == 3);
            assert!(
                !state.wrong.load(Ordering::Acquire),
                "refused decode input escaped CPU retirement"
            );
        } else {
            assert_eq!(state.count.load(Ordering::Acquire), 0);
        }
    }
    let host = migo_io::scheduler::IoScheduler::new(1);
    let (mut pool, _) = lazy(host.audio_cpu());
    let (tx, rx) = shared::audio_channel::channel();
    tx.try_send(AudioCmd::SetNodeParam {
        node_id: 1,
        param_name: "x".repeat(BYTES),
        value: 1.0,
    })
    .unwrap();
    let mut inbox = crate::command_inbox::Inbox::new(rx, vec![], host.audio_cpu());
    assert!(
        forward(
            inbox.next().unwrap(),
            &HashMap::new(),
            &mut HashMap::new(),
            &mut pool,
            &mut inbox
        )
        .is_some(),
        "decode dispatcher consumed unrelated command"
    );
    assert!(!pool.is_started());
}
