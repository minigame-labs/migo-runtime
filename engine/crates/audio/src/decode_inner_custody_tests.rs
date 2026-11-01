//! Original queue or completion tickets survive refused installation/publication.
use super::context_retirement_tests::{Dropped, block, wait, witness};
use super::*;
use crate::limits::AudioAggregateLedger;
use std::sync::atomic::Ordering;
const BYTES: usize = 4 << 20;
fn pcm() -> DecodedAudio {
    let mut samples = Vec::with_capacity(BYTES / 4);
    samples.extend((0..128).map(sample));
    DecodedAudio {
        samples,
        sample_rate: 48000,
        channels: 2,
    }
}
fn sample(i: usize) -> f32 {
    if i == 0 {
        -0.0
    } else {
        (i % 31) as f32 / 32.0 - 0.5
    }
}
fn small() -> DecodedAudio {
    DecodedAudio {
        samples: vec![0.25; 128],
        sample_rate: 48000,
        channels: 2,
    }
}
fn counters(tx: &DecodeQueueSender<DecodeResult>) -> (usize, usize) {
    (
        tx.usage.items.load(Ordering::Acquire),
        tx.usage.bytes.load(Ordering::Acquire),
    )
}
fn host() -> (
    migo_io::scheduler::IoScheduler,
    migo_io::audio_cpu::AudioCpu,
) {
    let h = migo_io::scheduler::IoScheduler::isolated_for_admission_probe(951, 1, u64::MAX);
    let cpu = h.audio_cpu();
    (h, cpu)
}
fn finish(tx: &DecodeQueueSender<DecodeResult>, seen: &Arc<Dropped>, n: usize) {
    wait(|| seen.count.load(Ordering::Acquire) == n && counters(tx) == (0, 0));
    assert!(
        !seen.wrong.load(Ordering::Acquire),
        "inner refused PCM escaped CPU workers"
    );
}
pub(super) fn custody() {
    for closed in [false, true] {
        for mode in [
            "missing",
            "stale",
            "foreign",
            "destroyed",
            "closed",
            "loaded",
            "quota",
            "success",
            "send-race",
            "error",
            "stale-error",
            "foreign-error",
        ] {
            installation(mode, closed);
        }
        for kind in ["buffer", "inner"] {
            for mode in [
                "full",
                "bytes",
                "disconnected",
                "cancelled",
                "generation",
                "success",
            ] {
                publication(kind, mode, closed);
            }
        }
    }
    legacy();
    for closed in [false, true] {
        for (managed, samples) in [(false, BYTES / 4), (true, 1023), (true, 1024)] {
            publication_boundary(managed, samples, closed);
        }
    }
}
fn installation(mode: &str, closed: bool) {
    let (host, cpu) = host();
    let (mut tx, rx) = result_custody::channel(1, BYTES, cpu.clone());
    let seen = Arc::new(Dropped::default());
    tx.retirement.as_mut().unwrap().hook = Some(witness(&seen));
    let ledger = Arc::new(AudioAggregateLedger::new(if mode == "quota" {
        0
    } else {
        2 * BYTES
    }));
    let mut player = InnerAudioPlayer::new_with_aggregate(1, 2, ledger.clone());
    let load = player.begin_load().unwrap();
    let preserve = matches!(
        mode,
        "stale" | "foreign" | "loaded" | "stale-error" | "foreign-error"
    );
    if mode == "loaded" {
        player.try_complete_load(&load, &mut small()).unwrap();
        player.drain_events().for_each(drop);
    }
    let mut players = HashMap::from([(1, player)]);
    let error = mode.ends_with("error");
    let data = if error {
        Err(EngineError::from_detail(
            ErrorCode::IoError,
            "original inner error",
        ))
    } else {
        Ok(pcm())
    };
    let pointer = data.as_ref().ok().map(|a| a.samples.as_ptr());
    let (resp, answer) = tokio::sync::oneshot::channel();
    let mut answer = Some(answer);
    publish_decode_result(
        &tx,
        DecodeResult::InnerAudio {
            id: 1,
            load,
            result: data,
            resp,
        },
    );
    let _former = if matches!(mode, "foreign" | "foreign-error") {
        let former = players.remove(&1);
        players.insert(
            1,
            InnerAudioPlayer::new_with_aggregate(1, 2, ledger.clone()),
        );
        former
    } else {
        None
    };
    if preserve && mode != "loaded" {
        let p = players.get_mut(&1).unwrap();
        let current = p.begin_load().unwrap();
        p.try_complete_load(&current, &mut small()).unwrap();
        p.drain_events().for_each(drop);
    }
    if matches!(mode, "missing" | "destroyed") {
        players.remove(&1);
    }
    // Publication cancellation is a different boundary, so missing/destroyed
    // fixtures above use their still-current token until after publication.
    if counters(&tx) == (0, 0) {
        panic!("installation fixture did not enter admitted result queue");
    }
    if mode == "closed" {
        drop(answer.take());
    }
    if mode == "send-race" {
        let answer = answer.take().unwrap();
        result_custody::BEFORE_SEND
            .with(|s| *s.borrow_mut() = Some(Box::new(move || drop(answer))));
    }
    let before_ledger = ledger.used_bytes();
    let gate = block(&cpu);
    if closed {
        host.close();
    }
    let before = migo_alloc_probe::thread_counts();
    result_custody::integrate(rx.try_recv_owned().unwrap(), &HashMap::new(), &mut players);
    let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
    let success = matches!(mode, "success" | "send-race");
    let current_error = mode == "error";
    if let Some(mut response) = answer {
        let response = response
            .try_recv()
            .expect("inner refusal delayed typed response");
        if success {
            let info = response.unwrap();
            assert_eq!(
                (info.sample_rate, info.channels, info.duration),
                (48000, 2, 64.0 / 48000.0),
                "inner installation changed metadata"
            );
        } else {
            let e = response.expect_err("inner installation changed typed refusal");
            let code = if mode == "quota" {
                ErrorCode::InputSaturated
            } else if current_error {
                ErrorCode::IoError
            } else {
                ErrorCode::Cancelled
            };
            assert_eq!(
                e.code, code,
                "inner integration changed refusal error precedence"
            );
            if current_error {
                assert_eq!(e.detail.as_deref(), Some("original inner error"));
            }
        }
    }
    assert!(
        freed < (BYTES / 2) as u64,
        "inner refused PCM escaped CPU workers"
    );
    if success {
        assert_eq!(
            counters(&tx),
            (0, 0),
            "inner installation retained transport credit after adoption"
        );
        assert_eq!(
            ledger.used_bytes(),
            BYTES,
            "inner installation lost persistent physical credit"
        );
        assert_eq!(seen.count.load(Ordering::Acquire), 0);
        let p = players.get_mut(&1).unwrap();
        assert_eq!(
            p.owned_samples_for_custody().as_ptr(),
            pointer.unwrap(),
            "inner installation copied original PCM"
        );
        assert!(
            p.owned_samples_for_custody()
                .iter()
                .enumerate()
                .all(|(i, v)| v.to_bits() == sample(i).to_bits()),
            "inner installation changed complete PCM"
        );
        assert_eq!(
            p.drain_events().count(),
            1,
            "inner installation changed CanPlay events"
        );
    } else if current_error {
        assert_eq!(counters(&tx), (0, 0));
        assert_eq!(seen.count.load(Ordering::Acquire), 0);
    } else {
        assert_eq!(
            counters(&tx),
            (1, if error { 0 } else { BYTES }),
            "inner refused result credit returned before CPU disposal"
        );
        assert_eq!(
            seen.count.load(Ordering::Acquire),
            0,
            "inner refused PCM escaped CPU workers"
        );
        assert!(
            matches!(tx.usage.try_reserve(0), Err(DecodeQueueReserveError::Full)),
            "inner refused result reopened original item admission"
        );
    }
    if !success {
        assert_eq!(
            ledger.used_bytes(),
            before_ledger,
            "inner refusal changed persistent allocation credit"
        );
    }
    if preserve {
        let p = players.get_mut(&1).unwrap();
        assert_eq!(p.drain_events().count(), 0);
        p.play();
        let mut output = [0.0; 8];
        assert!(p.process(&mut output));
        assert_eq!(
            output, [0.25; 8],
            "inner refusal changed the current source"
        );
    }
    gate.open();
    if !success && !current_error {
        finish(&tx, &seen, 1);
    }
}
fn publication(kind: &str, mode: &str, closed: bool) {
    let (host, cpu) = host();
    let (mut tx, rx) = result_custody::channel(
        1,
        if mode == "bytes" { BYTES - 1 } else { BYTES },
        cpu.clone(),
    );
    let seen = Arc::new(Dropped::default());
    tx.retirement.as_mut().unwrap().unadmitted_hook = Some(witness(&seen));
    let held = if mode == "full" {
        Some(tx.usage.try_reserve(0).ok().unwrap())
    } else {
        None
    };
    let mut receiver = Some(rx);
    if mode == "disconnected" {
        drop(receiver.take());
    }
    let budget = Arc::new(DecodeInFlightUsage::new(BYTES, BYTES));
    let permit = budget.try_reserve().unwrap();
    let slots = Arc::new(tokio::sync::Semaphore::new(1));
    let slot = slots.clone().try_acquire_owned().unwrap();
    let pending = Arc::new(DecodeQueueUsage {
        max_items: 1,
        max_bytes: 3,
        closed: std::sync::atomic::AtomicBool::new(false),
        items: 0.into(),
        bytes: 0.into(),
    });
    let waiting = pending.try_reserve(3).ok().unwrap();
    drop(waiting);
    let mut buffer_answer = None;
    let mut inner_answer = None;
    let result = if kind == "buffer" {
        let (resp, answer) = tokio::sync::oneshot::channel();
        if mode == "cancelled" {
            drop(answer);
        } else {
            buffer_answer = Some(answer);
        }
        DecodeResult::AudioBuffer {
            ctx_id: 1,
            result: Ok(pcm()),
            resp,
        }
    } else {
        let state = Arc::new(crate::inner_audio::InnerAudioSharedState::new());
        let load = state.begin_source(1).unwrap();
        if mode == "generation" {
            let _ = state.begin_source(2).unwrap();
        }
        let (resp, answer) = tokio::sync::oneshot::channel();
        if mode == "cancelled" {
            drop(answer);
        } else {
            inner_answer = Some(answer);
        }
        DecodeResult::InnerAudio {
            id: 1,
            load,
            result: Ok(pcm()),
            resp,
        }
    };
    let gate = block(&cpu);
    if closed {
        host.close();
    }
    let before = migo_alloc_probe::thread_counts();
    publish_decode_result_after(&tx, result, move || drop((permit, slot)));
    let success = mode == "success" || (mode == "generation" && kind == "buffer");
    if success {
        assert_eq!(
            budget.bytes.load(Ordering::Acquire),
            0,
            "successful publication retained old in-flight credit"
        );
        assert_eq!(pending.items.load(Ordering::Acquire), 0);
        assert_eq!(slots.available_permits(), 1);
        assert_eq!(
            counters(&tx),
            (1, BYTES),
            "successful publication lost result admission"
        );
        let entry = receiver.as_ref().unwrap().try_recv_owned().unwrap();
        assert!(entry.value().queued_bytes() == BYTES);
        drop(entry);
    } else {
        let expected = if mode == "cancelled" || mode == "generation" {
            ErrorCode::Cancelled
        } else if mode == "disconnected" {
            ErrorCode::Disconnected
        } else {
            ErrorCode::InputSaturated
        };
        if let Some(mut a) = buffer_answer {
            assert_eq!(
                a.try_recv().unwrap().unwrap_err().code,
                expected,
                "unadmitted publication changed typed refusal"
            );
        }
        if let Some(mut a) = inner_answer {
            assert_eq!(
                a.try_recv().unwrap().unwrap_err().code,
                expected,
                "unadmitted publication changed typed refusal"
            );
        }
        assert!(
            migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed < (BYTES / 2) as u64,
            "unadmitted PCM escaped CPU workers"
        );
        assert_eq!(
            budget.bytes.load(Ordering::Acquire),
            BYTES,
            "unadmitted completion credit returned before CPU disposal"
        );
        assert_eq!(
            pending.items.load(Ordering::Acquire),
            0,
            "publication extended already returned pending credit"
        );
        assert_eq!(
            slots.available_permits(),
            0,
            "unadmitted completion returned its local slot early"
        );
        assert!(
            budget.try_reserve().is_none(),
            "unadmitted completion reopened in-flight admission"
        );
        assert_eq!(
            seen.count.load(Ordering::Acquire),
            0,
            "unadmitted PCM escaped CPU workers"
        );
    }
    drop(held);
    gate.open();
    if !success {
        wait(|| seen.count.load(Ordering::Acquire) == 1);
        assert_eq!(
            budget.bytes.load(Ordering::Acquire),
            0,
            "unadmitted CPU disposal leaked completion credit"
        );
        assert!(
            !seen.wrong.load(Ordering::Acquire),
            "unadmitted PCM escaped CPU workers"
        );
    }
    wait(|| {
        counters(&tx) == (0, 0)
            && budget.bytes.load(Ordering::Acquire) == 0
            && pending.items.load(Ordering::Acquire) == 0
    });
}
fn legacy() {
    let (tx, rx) = decode_queue(1, BYTES);
    let mut p = InnerAudioPlayer::new(1, 2);
    let load = p.begin_load().unwrap();
    let (resp, answer) = tokio::sync::oneshot::channel();
    publish_decode_result(
        &tx,
        DecodeResult::InnerAudio {
            id: 1,
            load,
            result: Ok(pcm()),
            resp,
        },
    );
    drop(answer);
    let before = migo_alloc_probe::thread_counts();
    result_custody::integrate(
        rx.try_recv_owned().unwrap(),
        &HashMap::new(),
        &mut HashMap::from([(1, p)]),
    );
    assert_eq!(counters(&tx), (0, 0));
    assert!(migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed >= BYTES as u64);
}

fn publication_boundary(managed: bool, samples: usize, closed: bool) {
    let (host, cpu) = host();
    let (mut tx, _rx) = if managed {
        result_custody::channel(1, 0, cpu.clone())
    } else {
        decode_queue(1, 0)
    };
    let seen = Arc::new(Dropped::default());
    if let Some(retired) = tx.retirement.as_mut() {
        retired.unadmitted_hook = Some(witness(&seen));
    }
    let budget = Arc::new(DecodeInFlightUsage::new(BYTES, BYTES));
    let permit = budget.try_reserve().unwrap();
    let mut data = Vec::with_capacity(samples);
    data.extend((0..128).map(sample));
    let (resp, mut answer) = tokio::sync::oneshot::channel();
    let gate = block(&cpu);
    if closed {
        host.close();
    }
    publish_decode_result_after(
        &tx,
        DecodeResult::AudioBuffer {
            ctx_id: 1,
            result: Ok(DecodedAudio {
                samples: data,
                sample_rate: 48000,
                channels: 2,
            }),
            resp,
        },
        move || drop(permit),
    );
    assert_eq!(
        answer.try_recv().unwrap().unwrap_err().code,
        ErrorCode::InputSaturated
    );
    let deferred = managed && samples >= 1024;
    assert_eq!(
        budget.bytes.load(Ordering::Acquire),
        if deferred { BYTES } else { 0 },
        "unadmitted small/legacy capacity boundary changed"
    );
    assert_eq!(seen.count.load(Ordering::Acquire), 0);
    gate.open();
    if deferred {
        wait(|| seen.count.load(Ordering::Acquire) == 1);
        assert!(
            !seen.wrong.load(Ordering::Acquire),
            "unadmitted PCM escaped CPU workers"
        );
    }
    assert_eq!(budget.bytes.load(Ordering::Acquire), 0);
}
