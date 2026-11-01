use super::super::context_retirement_tests::{block, wait};
use super::*;
use crate::limits::{PcmBudget, PcmUsage};
use crate::nodes::AudioNodeProcessor;
const MAX: usize = 8 << 20;
fn graph(
    size: usize,
    cpu: &migo_io::audio_cpu::AudioCpu,
    ledger: &Arc<AudioAggregateLedger>,
) -> (
    HashMap<AudioContextId, AudioContext>,
    NodeContextIndex,
    AnalyserNode,
) {
    let budget = PcmBudget::with_aggregate(
        Arc::new(PcmUsage::new(MAX, 32)),
        Arc::new(PcmUsage::new(MAX, 32)),
        ledger.clone(),
    );
    let mut ctx = AudioContext::new_with_pcm_budget(1, 48000, 2, budget);
    ctx.prepare_node_retirement(cpu.clone(), ledger.clone())
        .unwrap();
    let mut actual = AnalyserNode::new(20, 2);
    let mut expected = AnalyserNode::new(20, 2);
    for node in [&mut actual, &mut expected] {
        node.set_fft_size(size);
        let input: Vec<_> = (0..size * 2)
            .map(|i| ((i as f32) * 0.17).sin() * 0.25)
            .collect();
        node.process(&input, &mut vec![0.0; input.len()], 48000, 2, 0.0);
    }
    assert!(ctx.add_node(Box::new(actual)));
    let mut index = NodeContextIndex::new();
    index.register(20, 1);
    (HashMap::from([(1, ctx)]), index, expected)
}
#[test]
fn all_analyser_shapes_keep_capacity_until_actual_storage_release() {
    for size in [32, 2048, 32768] {
        for kind in 0..4 {
            let scheduler = migo_io::scheduler::IoScheduler::new(1);
            let cpu = scheduler.audio_cpu();
            let ledger = Arc::new(AudioAggregateLedger::new(MAX));
            let (mut contexts, index, mut expected) = graph(size, &cpu, &ledger);
            let base = ledger.used_bytes();
            let gate = block(&cpu);
            let bytes = if kind < 2 {
                size / if kind == 1 { 2 } else { 1 }
            } else {
                size * 4 / if kind == 3 { 2 } else { 1 }
            };
            if kind < 2 {
                let data = super::bytes(&mut contexts, &index, 20, kind == 1).unwrap();
                let want = if kind == 1 {
                    expected.get_byte_frequency_data()
                } else {
                    expected.get_byte_time_domain_data()
                };
                assert_eq!(
                    data.as_ref(),
                    want,
                    "analyser complete byte snapshot changed"
                );
                assert!(
                    ledger.used_bytes() - base >= bytes + HANDOFF_BYTES,
                    "analyser output lacks physical capacity"
                );
                let (send, receive) = tokio::sync::oneshot::channel();
                send.send(data).unwrap();
                drop(receive);
            } else {
                let data = floats(&mut contexts, &index, 20, kind == 3).unwrap();
                let want = if kind == 3 {
                    expected.get_float_frequency_data()
                } else {
                    expected.get_float_time_domain_data()
                };
                assert!(
                    data.iter()
                        .zip(&want)
                        .all(|(a, b)| a.to_bits() == b.to_bits())
                        && data.len() == want.len(),
                    "analyser complete float snapshot changed"
                );
                assert!(
                    ledger.used_bytes() - base >= bytes + HANDOFF_BYTES,
                    "analyser output lacks physical capacity"
                );
                let (send, receive) = tokio::sync::oneshot::channel();
                drop(receive);
                drop(send.send(data));
            }
            if bytes >= 4096 {
                assert!(
                    ledger.used_bytes() - base >= bytes + HANDOFF_BYTES,
                    "analyser credit returned before CPU storage release"
                );
            } else {
                assert_eq!(
                    ledger.used_bytes(),
                    base,
                    "small analyser response did not release inline"
                );
            }
            gate.open();
            wait(|| ledger.used_bytes() == base);
            drop(contexts);
            scheduler.close();
            wait(|| ledger.used_bytes() == 0);
        }
    }
}
#[test]
fn analyser_refusal_precedes_allocation_and_preserves_smoothing_history() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(MAX));
    let (mut contexts, index, mut expected) = graph(32768, &cpu, &ledger);
    let base = ledger.used_bytes();
    let pressure = ledger.try_reserve(MAX - base, "peer").unwrap();
    let before = migo_alloc_probe::thread_counts();
    assert!(
        floats(&mut contexts, &index, 20, true).is_err(),
        "analyser ignored physical refusal"
    );
    assert!(
        migo_alloc_probe::thread_counts().bytes_allocated - before.bytes_allocated < 65536,
        "analyser refusal allocated output before admission"
    );
    assert_eq!(ledger.used_bytes(), MAX);
    drop(pressure);
    let data = floats(&mut contexts, &index, 20, true).unwrap();
    let want = expected.get_float_frequency_data();
    assert!(
        data.iter()
            .zip(want)
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "analyser refusal advanced FFT smoothing history"
    );
    drop(data);
    wait(|| ledger.used_bytes() == base);
    assert!(matches!(floats(&mut contexts,&index,999,false),Err(e) if e.code==ErrorCode::NotFound));
    drop(contexts);
    scheduler.close();
    wait(|| ledger.used_bytes() == 0);
}
#[test]
fn closed_analyser_host_refuses_new_storage_and_held_output_retires() {
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let cpu = scheduler.audio_cpu();
    let ledger = Arc::new(AudioAggregateLedger::new(MAX));
    let (mut contexts, index, _) = graph(32768, &cpu, &ledger);
    let data = floats(&mut contexts, &index, 20, false).unwrap();
    scheduler.close();
    assert!(
        floats(&mut contexts, &index, 20, false).is_err(),
        "closed analyser host admitted output"
    );
    assert_eq!(data.len(), 32768);
    drop(contexts);
    drop(data);
    wait(|| ledger.used_bytes() == 0);
}
