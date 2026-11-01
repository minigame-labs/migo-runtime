//! Two actual Host pools and each later PCM owner share one physical ticket.
use super::context_retirement_tests::{Dropped, block, wait, witness};
use super::*;
use shared::audio_resources::{AudioBufferFormat, AudioResourceRegistry};
use std::sync::atomic::Ordering;
const BYTES: usize = 4 << 20;
const WAIT: Duration = Duration::from_secs(15);
fn sample(i: usize) -> f32 {
    if i == 0 {
        -0.0
    } else {
        (i % 31) as f32 / 32.0 - 0.5
    }
}
fn pcm(full: bool) -> DecodedAudio {
    let mut samples = Vec::with_capacity(BYTES / 4);
    samples.extend((0..if full { BYTES / 4 } else { 128 }).map(sample));
    DecodedAudio {
        samples,
        sample_rate: 48000,
        channels: 2,
    }
}
fn response<T>(rx: tokio::sync::oneshot::Receiver<T>) -> T {
    crate::streaming::get_stream_runtime()
        .block_on(async { tokio::time::timeout(WAIT, rx).await.unwrap().unwrap() })
}
fn budget(ledger: &Arc<AudioAggregateLedger>) -> Arc<DecodeInFlightUsage> {
    Arc::new(DecodeInFlightUsage::new_with_aggregate(
        2 * BYTES,
        2 * BYTES,
        Some(ledger.clone()),
    ))
}
fn publish(
    tx: &DecodeQueueSender<DecodeResult>,
    budget: &Arc<DecodeInFlightUsage>,
    result: DecodeResult,
) {
    let mut permit = budget
        .try_reserve()
        .expect("physical test producer admission");
    publish_decode_result_handoff(tx, result, move || permit._aggregate.take());
}
fn counts(tx: &DecodeQueueSender<DecodeResult>) -> (usize, usize) {
    (
        tx.usage.items.load(Ordering::Acquire),
        tx.usage.bytes.load(Ordering::Acquire),
    )
}
fn same(audio: &[f32]) {
    assert!(
        audio
            .iter()
            .enumerate()
            .all(|(i, v)| v.to_bits() == sample(i).to_bits()),
        "physical handoff changed complete PCM"
    );
}
pub(super) fn handoff() {
    actual_pools();
    for closed in [false, true] {
        for mode in [
            "queued",
            "receiver-drop",
            "response",
            "unpolled",
            "adopted",
            "spare",
            "registry-refused",
            "missing-context",
            "inner",
            "inner-refused",
        ] {
            lifetime(mode, closed);
        }
        for mode in ["cancelled", "queue-full", "byte-full", "disconnected"] {
            unadmitted(mode, closed);
        }
    }
    foreign_ledger();
    shared_registry();
    foreign_registry();
    initial_service();
    raw_frozen_service();
}
fn published(rx: &DecodeQueueReceiver<DecodeResult>) -> result_custody::Owned<DecodeResult> {
    let mut entry = None;
    wait(|| {
        entry = rx.try_recv_owned().ok();
        entry.is_some()
    });
    entry.unwrap()
}
fn actual_pools() {
    let first = migo_io::scheduler::IoScheduler::isolated_for_admission_probe(971, 1, u64::MAX);
    let second = migo_io::scheduler::IoScheduler::isolated_for_admission_probe(972, 1, u64::MAX);
    let ledger = Arc::new(AudioAggregateLedger::new(65536));
    let budget = Arc::new(DecodeInFlightUsage::new_with_aggregate(
        65536,
        32768,
        Some(ledger.clone()),
    ));
    let (a, ar) = result_custody::channel(8, 65536, first.audio_cpu());
    let (b, br) = result_custody::channel(8, 65536, second.audio_cpu());
    let mut p = DecodePool::new_with_in_flight(
        1,
        a.clone(),
        budget.clone(),
        ThreadWakeup::new(),
        first.audio_cpu(),
    );
    let mut q = DecodePool::new_with_in_flight(
        1,
        b.clone(),
        budget.clone(),
        ThreadWakeup::new(),
        second.audio_cpu(),
    );
    let data = tests::wav(8000, 4096);
    let mut replies = Vec::new();
    let mut visible = Vec::new();
    for (pool, tx, rx, expected) in [(&mut p, &a, &ar, 16384), (&mut q, &b, &br, 32768)] {
        let (mut job, reply) = tests::audio_buffer_decode_job(1, Arc::new(data.clone()));
        if let DecodeJob::AudioBuffer { target_rate, .. } = &mut job {
            *target_rate = 8000;
        }
        replies.push(reply);
        pool.submit(job);
        wait(|| {
            counts(tx).0 == 1
                && budget.bytes.load(Ordering::Acquire) == 0
                && pool.slots.available_permits() == 1
        });
        // Queue usage counts admission before the result becomes visible.
        // Receiving the actual owner is the publication boundary.
        visible.push(published(rx));
        assert_eq!(
            ledger.used_bytes(),
            expected,
            "actual pool publication lost physical PCM credit"
        );
    }
    let foreign = ledger.try_reserve(32768, "other PCM domain").unwrap();
    let (mut job, reply) = tests::audio_buffer_decode_job(1, Arc::new(data.clone()));
    if let DecodeJob::AudioBuffer { target_rate, .. } = &mut job {
        *target_rate = 8000;
    }
    q.submit(job);
    let refused = published(&br);
    assert_eq!(counts(&b).0, 2);
    drop(visible.pop().unwrap());
    let refused = refused.into_value();
    let DecodeResult::AudioBuffer { result, .. } = refused else {
        panic!("buffer")
    };
    assert_eq!(
        result.unwrap_err().code,
        ErrorCode::InputSaturated,
        "cross-host retained PCM reopened decode admission"
    );
    drop(reply);
    drop(foreign);
    let contexts = HashMap::from([(1, AudioContext::new(1, 8000, 1))]);
    let expected = crate::decoder::decode(&data).unwrap();
    result_custody::integrate(visible.pop().unwrap(), &contexts, &mut HashMap::new());
    let pcm = response(replies.remove(0)).unwrap();
    assert_eq!((pcm.sample_rate, pcm.channels, pcm.frames), (8000, 1, 4096));
    assert!(
        pcm.samples
            .iter()
            .zip(expected.samples)
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "actual pool complete PCM changed"
    );
    first.close();
    second.close();
    drop(pcm);
    drop(replies);
    drop(p);
    drop(q);
    drop(ar);
    drop(br);
    wait(|| ledger.used_bytes() == 0);
}
fn lifetime(mode: &str, closed: bool) {
    let host = migo_io::scheduler::IoScheduler::isolated_for_admission_probe(973, 1, u64::MAX);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(2 * BYTES));
    let producer = budget(&ledger);
    let (mut tx, rx) = result_custody::channel(1, BYTES, cpu.clone());
    let seen = Arc::new(Dropped::default());
    tx.retirement.as_mut().unwrap().hook = Some(witness(&seen));
    let data = pcm(mode != "spare");
    let ptr = data.samples.as_ptr();
    let frames = data.frame_count() as u32;
    let mut player = crate::inner_audio::physical_custody_player(1, cpu.clone(), ledger.clone());
    let load = player.begin_load_for(1).unwrap();
    let (resp, answer) = tokio::sync::oneshot::channel();
    let (inner_resp, inner_answer) = tokio::sync::oneshot::channel();
    let result = if mode.starts_with("inner") {
        DecodeResult::InnerAudio {
            id: 1,
            load,
            result: Ok(data),
            resp: inner_resp,
        }
    } else {
        DecodeResult::AudioBuffer {
            ctx_id: 1,
            result: Ok(data),
            resp,
        }
    };
    publish(&tx, &producer, result);
    if mode == "inner-refused" {
        let _ = player.begin_load_for(2).unwrap();
    }

    assert_eq!(
        producer.bytes.load(Ordering::Acquire),
        0,
        "physical handoff retained inflight domain credit"
    );
    assert_eq!(
        ledger.used_bytes(),
        BYTES,
        "queued PCM lost physical credit or kept scratch reservation"
    );
    assert_eq!(counts(&tx), (1, BYTES));
    // Fill the shared ledger using a separate domain. A transferred InnerAudio
    // source must still install without reserving a second physical copy.
    let other = ledger.try_reserve(BYTES, "other PCM domain").unwrap();
    assert!(
        ledger.try_reserve(1, "peer host").is_err(),
        "queued PCM reopened physical admission"
    );
    let gate = block(&cpu);
    if closed {
        host.close();
        drop(host);
    }
    let before = migo_alloc_probe::thread_counts();
    if mode == "receiver-drop" {
        drop(rx);
    } else {
        let entry = rx.try_recv_owned().unwrap();
        if mode == "queued" {
            drop(entry);
        } else {
            let contexts = if mode == "missing-context" {
                HashMap::new()
            } else {
                HashMap::from([(1, AudioContext::new(1, 48000, 2))])
            };
            let mut players = HashMap::from([(1, player)]);
            result_custody::integrate(entry, &contexts, &mut players);
            if mode.starts_with("inner") {
                if mode == "inner-refused" {
                    assert_eq!(
                        response(inner_answer).unwrap_err().code,
                        ErrorCode::Cancelled
                    );
                } else {
                    assert!(
                        response(inner_answer).is_ok(),
                        "inner physical ticket was charged twice"
                    );
                    assert_eq!(
                        players[&1].owned_samples_for_custody().as_ptr(),
                        ptr,
                        "inner physical handoff copied PCM"
                    );
                    same(players[&1].owned_samples_for_custody());
                    assert_eq!(
                        counts(&tx),
                        (0, 0),
                        "inner physical handoff retained result ticket"
                    );
                    assert_eq!(
                        ledger.used_bytes(),
                        2 * BYTES,
                        "inner physical handoff lost or duplicated credit"
                    );
                }
                drop(players);
            } else if mode == "missing-context" {
                assert_eq!(response(answer).unwrap_err().code, ErrorCode::NotFound);
            } else if mode == "unpolled" {
                drop(answer);
            } else {
                let pcm = response(answer).unwrap();
                same(&pcm.samples);
                assert_eq!(
                    pcm.samples.as_ptr(),
                    ptr,
                    "response physical handoff copied PCM"
                );
                assert_eq!(
                    (pcm.sample_rate, pcm.channels, pcm.frames),
                    (48000, 2, frames)
                );
                if matches!(mode, "adopted" | "spare" | "registry-refused") {
                    let registry = AudioResourceRegistry::new();
                    let format = AudioBufferFormat {
                        sample_rate: 48000,
                        channels: 2,
                        frames,
                    };
                    if mode == "registry-refused" {
                        registry.finish_runtime_drop(77);
                    }
                    let adopted = registry.adopt_decoded(77, format, pcm);
                    assert_eq!(
                        counts(&tx),
                        (0, 0),
                        "registry adoption retained result ticket"
                    );
                    assert_eq!(
                        ledger.used_bytes(),
                        2 * BYTES,
                        "registry adoption returned physical credit"
                    );
                    if mode == "registry-refused" {
                        assert_eq!(adopted.unwrap_err().code, ErrorCode::InvalidOperation);
                    } else {
                        let handle = adopted.unwrap();
                        let key = handle.key();
                        let snapshot = registry.prepare_snapshot(key, None).unwrap().snapshot();
                        same(snapshot.samples());
                        if mode != "spare" {
                            assert_eq!(
                                snapshot.samples().as_ptr(),
                                ptr,
                                "registry physical handoff copied PCM"
                            );
                        }
                        registry.release_buffer(key);
                        drop(registry);
                        assert_eq!(
                            ledger.used_bytes(),
                            2 * BYTES,
                            "external snapshot lost physical credit"
                        );
                        drop(snapshot);
                    }
                } else {
                    drop(pcm);
                }
            }
        }
    }
    assert!(
        migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed < (BYTES / 2) as u64
            || mode == "spare",
        "physical PCM destructor escaped CPU workers"
    );
    assert_eq!(
        ledger.used_bytes(),
        2 * BYTES,
        "physical PCM credit returned before CPU disposal"
    );
    assert!(
        ledger.try_reserve(1, "peer host").is_err(),
        "CPU-pending PCM reopened physical admission"
    );
    gate.open();
    wait(|| ledger.used_bytes() == BYTES);
    if !mode.starts_with("inner") {
        wait(|| seen.count.load(Ordering::Acquire) == 1);
        assert!(
            !seen.wrong.load(Ordering::Acquire),
            "physical PCM disposal escaped CPU workers"
        );
    }
    assert_eq!(
        counts(&tx),
        (0, 0),
        "physical retirement leaked result credit"
    );
    drop(other);
    assert_eq!(
        ledger.used_bytes(),
        0,
        "physical retirement leaked aggregate credit"
    );
}
fn unadmitted(mode: &str, closed: bool) {
    let host = migo_io::scheduler::IoScheduler::isolated_for_admission_probe(974, 1, u64::MAX);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(2 * BYTES));
    let producer = budget(&ledger);
    let (tx, rx) = result_custody::channel(
        if mode == "queue-full" { 0 } else { 1 },
        if mode == "byte-full" {
            BYTES - 1
        } else {
            BYTES
        },
        cpu.clone(),
    );
    if mode == "disconnected" {
        drop(rx);
    }
    let (resp, answer) = tokio::sync::oneshot::channel();
    if mode == "cancelled" {
        drop(answer);
    } // The response itself need not be polled to retire.
    let gate = block(&cpu);
    if closed {
        host.close();
        drop(host);
    }
    publish(
        &tx,
        &producer,
        DecodeResult::AudioBuffer {
            ctx_id: 1,
            result: Ok(pcm(false)),
            resp,
        },
    );
    assert_eq!(
        producer.bytes.load(Ordering::Acquire),
        2 * BYTES,
        "unadmitted result returned inflight credit before CPU disposal"
    );
    assert_eq!(
        ledger.used_bytes(),
        2 * BYTES,
        "unadmitted result returned physical credit before CPU disposal"
    );
    gate.open();
    wait(|| ledger.used_bytes() == 0 && producer.bytes.load(Ordering::Acquire) == 0);
}
fn foreign_ledger() {
    let foreign = Arc::new(AudioAggregateLedger::new(BYTES));
    let local = Arc::new(AudioAggregateLedger::new(0));
    let mut player = InnerAudioPlayer::new_with_aggregate(1, 2, local);
    let load = player.begin_load_for(1).unwrap();
    let mut data = pcm(false);
    let ptr = data.samples.as_ptr();
    let mut physical = Some(foreign.try_reserve(BYTES, "foreign").unwrap());
    let result = player.try_complete_load_charged(&load, &mut data, &mut physical);
    assert!(
        matches!(result,Err(error) if error.code==ErrorCode::InputSaturated),
        "foreign physical ticket bypassed destination ledger"
    );
    assert_eq!(data.samples.as_ptr(), ptr);
    assert!(physical.is_some());
    assert_eq!(foreign.used_bytes(), BYTES);
    drop(data);
    drop(physical);
    assert_eq!(foreign.used_bytes(), 0);
}

fn shared_registry() {
    assert!(
        Arc::ptr_eq(
            &AudioAggregateLedger::process_global(),
            &shared::audio_physical::AudioAggregateLedger::process_global()
        ),
        "audio and registry process physical ledgers diverged"
    );
    let host = migo_io::scheduler::IoScheduler::isolated_for_admission_probe(975, 1, u64::MAX);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(2 * BYTES));
    let producer = budget(&ledger);
    let registry = AudioResourceRegistry::with_physical_ledger(ledger.clone());
    let (tx, rx) = result_custody::channel(1, BYTES, cpu.clone());
    let (resp, answer) = tokio::sync::oneshot::channel();
    publish(
        &tx,
        &producer,
        DecodeResult::AudioBuffer {
            ctx_id: 1,
            result: Ok({
                let mut data = pcm(true);
                data.channels = 1;
                data
            }),
            resp,
        },
    );
    let contexts = HashMap::from([(1, AudioContext::new(1, 48000, 2))]);
    result_custody::integrate(rx.try_recv_owned().unwrap(), &contexts, &mut HashMap::new());
    let credit = ledger.try_reserve(BYTES, "peer domain").unwrap();
    let pcm = response(answer).unwrap();
    let format = AudioBufferFormat {
        sample_rate: 48000,
        channels: 1,
        frames: (BYTES / 4) as u32,
    };
    let adopted = registry.adopt_decoded(77, format, pcm);
    assert!(adopted.is_ok(), "registry decoded PCM was charged twice");
    let key = adopted.unwrap().key();
    assert_eq!(
        ledger.used_bytes(),
        2 * BYTES,
        "registry decoded adoption lost original physical credit"
    );
    assert_eq!(counts(&tx), (0, 0));
    assert!(
        registry.prepare_materialize(key).is_err(),
        "registry materialize bypassed another audio domain"
    );
    drop(credit);
    let node = registry.prepare_snapshot(key, None).unwrap().snapshot();
    let native = registry
        .prepare_materialize(key)
        .expect("registry materialize remained saturated after peer credit returned")
        .commit_owned();
    assert_eq!(
        ledger.used_bytes(),
        2 * BYTES,
        "registry native overlap missing from audio ledger"
    );
    same(native.samples());
    let refused = registry.prepare_snapshot(key, Some(native.samples()));
    assert!(
        refused.is_err(),
        "registry freeze bypassed live materialized physical credit"
    );
    registry.release_buffer(key);
    drop(registry);
    host.close();
    drop(host);
    drop(native);
    same(node.samples());
    drop(node);
    wait(|| ledger.used_bytes() == 0);
    assert_eq!(
        ledger.used_bytes(),
        0,
        "registry mixed-domain physical credit leaked"
    );
}
fn foreign_registry() {
    let host = migo_io::scheduler::IoScheduler::isolated_for_admission_probe(976, 1, u64::MAX);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(BYTES * 2));
    let producer = budget(&ledger);
    let registry =
        AudioResourceRegistry::with_physical_ledger(Arc::new(AudioAggregateLedger::new(0)));
    let (tx, rx) = result_custody::channel(1, BYTES, cpu.clone());
    let (resp, answer) = tokio::sync::oneshot::channel();
    publish(
        &tx,
        &producer,
        DecodeResult::AudioBuffer {
            ctx_id: 1,
            result: Ok(pcm(true)),
            resp,
        },
    );
    let contexts = HashMap::from([(1, AudioContext::new(1, 48000, 2))]);
    result_custody::integrate(rx.try_recv_owned().unwrap(), &contexts, &mut HashMap::new());
    let pcm = response(answer).unwrap();
    let gate = block(&cpu);
    let result = registry.adopt_decoded(
        77,
        AudioBufferFormat {
            sample_rate: 48000,
            channels: 2,
            frames: (BYTES / 8) as u32,
        },
        pcm,
    );
    assert!(
        matches!(result,Err(error) if error.code==ErrorCode::InputSaturated),
        "foreign registry bypassed destination physical admission"
    );
    assert_eq!(
        ledger.used_bytes(),
        BYTES,
        "foreign registry returned source credit before CPU disposal"
    );
    gate.open();
    wait(|| ledger.used_bytes() == 0);
}
fn initial_service() {
    let ledger = Arc::new(AudioAggregateLedger::new(BYTES));
    let registry = AudioResourceRegistry::with_physical_ledger(ledger.clone());
    let (commands, _receiver) = shared::audio_channel::channel();
    let tx = shared::op_state::AudioSender::hosted(
        commands,
        shared::channel::ThreadWakeup::new(),
        shared::op_state::AudioHostStartSignal::new(),
        registry.clone(),
    );
    let scope = migo_services::audio::BufferScope {
        tx: &tx,
        runtime_generation: 77,
    };
    for raw in [false, true] {
        let id = migo_services::audio::reserve_buffer(scope, 2, (BYTES / 8) as u32, 48000).unwrap();
        if raw {
            let result = migo_services::audio::materialize_buffer(scope, id);
            assert!(
                result.is_ok(),
                "external initial service did not initialize pending backing"
            );
            let data = result.unwrap();
            assert_eq!(data.len(), BYTES / 4);
            assert!(data.iter().all(|v| v.to_bits() == 0));
            migo_services::audio::release_buffer(scope, id);
            assert_eq!(
                ledger.used_bytes(),
                BYTES,
                "raw service released live backing credit"
            );
            drop(data);
        } else {
            let result = migo_services::audio::materialize_owned_buffer(scope, id);
            assert!(
                result.is_ok(),
                "embedded initial service did not initialize pending backing"
            );
            let data = result.unwrap();
            assert_eq!(data.samples().len(), BYTES / 4);
            assert!(data.samples().iter().all(|v| v.to_bits() == 0));
            migo_services::audio::release_buffer(scope, id);
            assert_eq!(
                ledger.used_bytes(),
                BYTES,
                "initial service released live owned credit"
            );
            drop(data);
        }
        assert_eq!(ledger.used_bytes(), 0);
    }
}

fn raw_frozen_service() {
    let ledger = Arc::new(AudioAggregateLedger::new(48));
    let registry = AudioResourceRegistry::with_physical_ledger(ledger.clone());
    let (commands, _receiver) = shared::audio_channel::channel();
    let tx = shared::op_state::AudioSender::hosted(
        commands,
        ThreadWakeup::new(),
        shared::op_state::AudioHostStartSignal::new(),
        registry.clone(),
    );
    let scope = migo_services::audio::BufferScope {
        tx: &tx,
        runtime_generation: 77,
    };
    let key = registry
        .adopt_frozen(
            77,
            AudioBufferFormat {
                channels: 2,
                frames: 3,
                sample_rate: 48000,
            },
            vec![-0.0, 1.0, 2.0, 3.0, 4.0, 5.0],
        )
        .unwrap()
        .key();
    let result = migo_services::audio::materialize_buffer(scope, key.serial);
    assert!(
        result.is_ok(),
        "raw frozen service did not materialize backing"
    );
    let mut data = result.unwrap();
    assert_eq!(
        data.iter().map(|f| f.to_bits()).collect::<Vec<_>>(),
        [-0.0f32, 2.0, 4.0, 1.0, 3.0, 5.0].map(f32::to_bits),
        "raw frozen service changed complete PCM"
    );
    let before = migo_alloc_probe::thread_counts();
    data[0] = 9.0;
    assert_eq!(
        migo_alloc_probe::thread_counts().bytes_allocated,
        before.bytes_allocated,
        "raw sample mutation allocated replacement storage"
    );
    migo_services::audio::release_buffer(scope, key.serial);
    drop(tx);
    drop(registry);
    assert_eq!(
        ledger.used_bytes(),
        24,
        "raw frozen service released live backing credit"
    );
    assert_eq!(data[0], 9.0);
    drop(data);
    assert_eq!(
        ledger.used_bytes(),
        0,
        "raw frozen service final drop leaked credit"
    );
}
