//! Physical output credit follows the real arrays through every response owner.
use super::super::context_retirement_tests::{Dropped, block, wait, witness};
use super::*;
use crate::limits::{AudioAggregateLedger, PcmBudget, PcmUsage};
use std::sync::atomic::Ordering;
const MAX: usize = 8 << 20;
fn graph(
    cpu: &migo_io::audio_cpu::AudioCpu,
    ledger: &Arc<AudioAggregateLedger>,
    rate: u32,
) -> (HashMap<AudioContextId, AudioContext>, NodeContextIndex) {
    let budget = PcmBudget::with_aggregate(
        Arc::new(PcmUsage::new(MAX, 32)),
        Arc::new(PcmUsage::new(MAX, 32)),
        ledger.clone(),
    );
    let mut ctx = AudioContext::new_with_pcm_budget(1, rate, 2, budget);
    ctx.prepare_node_retirement(cpu.clone(), ledger.clone())
        .unwrap();
    assert!(ctx.add_node(Box::new(BiquadFilterNode::new(20, 2, rate))));
    assert!(ctx.add_node(Box::new(IIRFilterNode::new(
        30,
        vec![0.5, 0.1],
        vec![1.0, -0.2],
        2
    ))));
    let mut index = NodeContextIndex::new();
    index.register(20, 1);
    index.register(30, 1);
    (HashMap::from([(1, ctx)]), index)
}
fn expected(id: u32, rate: u32, frequencies: &[f32]) -> Data {
    if id == 20 {
        BiquadFilterNode::new(20, 2, rate).get_frequency_response(rate as f64, frequencies)
    } else {
        IIRFilterNode::new(30, vec![0.5, 0.1], vec![1.0, -0.2], 2)
            .get_frequency_response(rate as f64, frequencies)
    }
}
fn values(len: usize) -> Vec<f32> {
    let mut v: Vec<_> = (0..len).map(|i| (i % 79) as f32 * 377.0).collect();
    for (slot, v) in v.iter_mut().zip([-0.0, f32::NAN, f32::INFINITY, -100.0]) {
        *slot = v;
    }
    v
}
fn matches(response: &FrequencyResponse, expected: &Data) -> bool {
    let (a, b) = response.values();
    [a, b]
        .into_iter()
        .zip([expected.0.as_slice(), expected.1.as_slice()])
        .all(|(a, b)| {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.to_bits() == b.to_bits())
        })
}
fn caller_witness(state: &Arc<Dropped>) -> Arc<dyn Fn() + Send + Sync> {
    let state = state.clone();
    let caller = std::thread::current().id();
    Arc::new(move || {
        if std::thread::current().id() != caller {
            state.wrong.store(true, Ordering::Release);
        }
        state.count.fetch_add(1, Ordering::Release);
    })
}
pub(crate) fn outputs() {
    for rate in [44100, 48000] {
        let host = migo_io::scheduler::IoScheduler::new(1);
        let cpu = host.audio_cpu();
        let ledger = Arc::new(AudioAggregateLedger::new(MAX));
        let (mut contexts, index) = graph(&cpu, &ledger, rate);
        let base = ledger.used_bytes();
        for id in [20, 30] {
            for len in [0, 511, 512, 16384] {
                let frequencies = values(len);
                let want = expected(id, rate, &frequencies);
                let bytes = len * 8;
                for mode in 0..4 {
                    let state = Arc::new(Dropped::default());
                    let large = bytes >= 4096;
                    let hook = if large {
                        witness(&state)
                    } else {
                        caller_witness(&state)
                    };
                    let gate = block(&cpu);
                    let response =
                        observe_output(hook, || query(&mut contexts, &index, id, &frequencies))
                            .unwrap();
                    assert!(
                        matches(&response, &want),
                        "owned output changed complete frequency results"
                    );
                    let cost = ledger.used_bytes() - base;
                    assert!(
                        cost >= bytes + HANDOFF_BYTES && cost <= bytes + HANDOFF_BYTES + 1024,
                        "response storage was not fully admitted before allocation"
                    );
                    // Borrowing the complete arrays, as either serializer does,
                    // cannot detach their physical ticket from the response.
                    let wire: Vec<Vec<u8>> = [response.values().0, response.values().1]
                        .into_iter()
                        .map(|v| v.iter().flat_map(|v| v.to_le_bytes()).collect())
                        .collect();
                    assert_eq!(wire.iter().map(Vec::len).sum::<usize>(), bytes);
                    assert_eq!(
                        ledger.used_bytes() - base,
                        cost,
                        "borrowing output released physical credit"
                    );
                    let before_drop = migo_alloc_probe::thread_counts();
                    match mode {
                        0 => drop(response),
                        1 => {
                            let (send, answer) = tokio::sync::oneshot::channel();
                            send.send(response).unwrap();
                            drop(answer);
                        }
                        2 => {
                            let (send, answer) = tokio::sync::oneshot::channel();
                            drop(answer);
                            let _ = send.send(response);
                        }
                        _ => {
                            let (send, mut answer) = tokio::sync::oneshot::channel();
                            send.send(response).unwrap();
                            let held = answer.try_recv().unwrap();
                            assert_eq!(ledger.used_bytes() - base, cost);
                            drop(held);
                        }
                    }
                    let caller_freed =
                        migo_alloc_probe::thread_counts().bytes_freed - before_drop.bytes_freed;
                    let observed = state.count.load(Ordering::Acquire);
                    let held = ledger.used_bytes() - base;
                    gate.open();
                    if large {
                        assert!(
                            caller_freed < bytes as u64,
                            "large response arrays freed on caller"
                        );
                        wait(|| state.count.load(Ordering::Acquire) == 1);
                        assert!(
                            !state.wrong.load(Ordering::Acquire),
                            "large response escaped CPU retirement"
                        );
                        assert!(
                            observed == 0 && held >= bytes + HANDOFF_BYTES,
                            "output credit returned before CPU drop"
                        );
                    } else {
                        assert!(
                            observed == 1 && held == 0,
                            "small output cleanup left caller"
                        );
                        assert!(
                            !state.wrong.load(Ordering::Acquire),
                            "small output cleanup left caller"
                        );
                    }
                    wait(|| ledger.used_bytes() == base);
                }
            }
        }
        contexts.clear();
        drop(host);
        wait(|| ledger.used_bytes() == 0);
    }
    refusal_and_close();
}
fn refusal_and_close() {
    let host = migo_io::scheduler::IoScheduler::new(1);
    let cpu = host.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(MAX));
    let (mut contexts, index) = graph(&cpu, &ledger, 48000);
    let base = ledger.used_bytes();
    let frequencies = values(16384);
    let response = query(&mut contexts, &index, 20, &frequencies).unwrap();
    let cost = ledger.used_bytes() - base;
    drop(response);
    wait(|| ledger.used_bytes() == base);
    for remaining in [0, cost - 1] {
        contexts.get_mut(&1).unwrap().collect_device_controls();
        let fill = ledger
            .try_reserve(MAX - base - remaining, "response test blocker")
            .unwrap();
        let before = migo_alloc_probe::thread_counts();
        let result = query(&mut contexts, &index, 20, &frequencies);
        let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated;
        assert!(
            result.is_err_and(|e| e.code == ErrorCode::InputSaturated),
            "output allocation bypassed physical quota refusal"
        );
        assert!(
            allocated < 4096 && !contexts.get(&1).unwrap().has_device_controls(),
            "refused output allocated arrays or updated filter coefficients"
        );
        assert_eq!(ledger.used_bytes(), MAX - remaining);
        drop(fill);
    }
    contexts.get_mut(&1).unwrap().collect_device_controls();
    let oversized = values(migo_services::audio::MAX_FREQUENCY_RESPONSE_POINTS + 1);
    let before = migo_alloc_probe::thread_counts();
    let result = query(&mut contexts, &index, 20, &oversized);
    assert!(
        result.is_err_and(|e| e.code == ErrorCode::InputSaturated),
        "native response bypassed existing public point limit"
    );
    assert!(
        migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated < 4096
            && !contexts.get(&1).unwrap().has_device_controls(),
        "oversized output allocated arrays before refusal"
    );
    let state = Arc::new(Dropped::default());
    let gate = block(&cpu);
    let response = observe_output(witness(&state), || {
        query(&mut contexts, &index, 20, &frequencies)
    })
    .unwrap();
    let held = ledger.used_bytes();
    host.close();
    contexts.get_mut(&1).unwrap().collect_device_controls();
    let before = migo_alloc_probe::thread_counts();
    let result = query(&mut contexts, &index, 20, &frequencies);
    assert!(
        result.is_err_and(|e| e.code == ErrorCode::IoError),
        "closed host admitted new response storage"
    );
    assert!(
        migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated < 4096
            && !contexts.get(&1).unwrap().has_device_controls(),
        "closed response allocated arrays or updated filter coefficients"
    );
    assert_eq!(ledger.used_bytes(), held);
    contexts.clear();
    drop(host);
    assert!(
        response.values().0.len() == 16384 && ledger.used_bytes() >= cost,
        "context close detached a held response ticket"
    );
    drop(response);
    let retained = ledger.used_bytes() >= cost;
    gate.open();
    wait(|| state.count.load(Ordering::Acquire) == 1);
    assert!(
        !state.wrong.load(Ordering::Acquire) && retained,
        "closed-host response escaped CPU ownership"
    );
    wait(|| ledger.used_bytes() == 0);
}
