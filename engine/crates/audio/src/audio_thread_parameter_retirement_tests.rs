//! Parameter setters overwrote FFT/FIR tables on the audio command caller.
use super::context_retirement_tests::{Dropped, block, context, wait, witness};
use super::*;
use crate::limits::{AudioAggregateLedger, PcmUsage};
use crate::nodes::AudioNodeProcessor;
use std::sync::atomic::Ordering;

const LIMIT: usize = 32 << 20;

fn pcm_eq(actual: &[f32], expected: &[f32]) {
    assert_eq!(
        actual.len(),
        expected.len(),
        "parameter replacement PCM shape changed"
    );
    assert!(
        actual
            .iter()
            .zip(expected)
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "parameter replacement PCM or filter history changed"
    );
}

pub(crate) fn analyser_tables() {
    for channels in [1, 2, 32] {
        let host = migo_io::scheduler::IoScheduler::new(1);
        let cpu = host.audio_cpu();
        let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
        let usage = Arc::new(PcmUsage::new(16 << 20, 16));
        let process = Arc::new(PcmUsage::new(16 << 20, 16));
        let mut ctx = context(&cpu, &ledger, &usage, &process, channels);
        let mut original = AnalyserNode::new(10, channels);
        let mut reference = AnalyserNode::new(11, channels);
        original.set_fft_size(32768);
        reference.set_fft_size(32768);
        assert!(ctx.add_node(Box::new(original)));
        let input: Vec<f32> = (0..128 * channels)
            .map(|i| (i % 37) as f32 / 37.0 - 0.5)
            .collect();
        let mut actual = vec![0.0; input.len()];
        let mut expected = actual.clone();
        for (size, deny) in [
            (32000_usize, false),
            (0, false),
            (33, false),
            (1025, false),
            (32768, false),
            (32, true),
        ] {
            let old_size = reference.get_float_time_domain_data().len();
            let needs_job = size.next_power_of_two().clamp(32, 32768) != old_size;
            let bytes = old_size * 40;
            let next_bytes = size.next_power_of_two().clamp(32, 32768) * 40;
            let retire = needs_job;
            let filler = deny.then(|| {
                ledger
                    .try_reserve(LIMIT - ledger.used_bytes(), "test FFT table quota")
                    .unwrap()
            });
            let state = Arc::new(Dropped::default());
            let gate = block(&cpu);
            let credit = ledger.used_bytes();
            let before = migo_alloc_probe::thread_counts();
            let accepted = crate::node_retirement::observe_replacement(witness(&state), || {
                ctx.set_analyser_fft_size(10, size)
            });
            let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
            let held = ledger.used_bytes();
            let queued = state.count.load(Ordering::Acquire);
            gate.open();
            if bytes >= crate::node_retirement::HANDOFF_BYTES {
                assert!(
                    freed < (bytes / 2) as u64,
                    "FFT tables escaped CPU retirement"
                );
            } else {
                assert!(
                    freed <= (bytes + 1024) as u64,
                    "small FFT cleanup exceeded its bound"
                );
            }
            assert_eq!(accepted, !deny, "FFT table admission changed old state");
            assert_eq!(queued, 0, "FFT tables escaped CPU retirement");
            if accepted && retire {
                assert!(
                    held >= credit + next_bytes + 4096,
                    "FFT table capacity escaped physical ledger"
                );
                wait(|| {
                    state.count.load(Ordering::Acquire) == 1
                        && ledger.used_bytes() == credit - bytes + next_bytes
                });
                assert!(
                    !state.wrong.load(Ordering::Acquire),
                    "FFT tables escaped CPU retirement"
                );
            } else {
                assert_eq!(held, credit, "unchanged/refused FFT retired live tables");
                assert_eq!(state.count.load(Ordering::Acquire), 0);
            }
            if accepted {
                reference.set_fft_size(size);
            }
            ctx.with_node_typed::<AnalyserNode, _>(10, |node| {
                // Check retained history before producing a new block too.
                pcm_eq(
                    &node.get_float_time_domain_data(),
                    &reference.get_float_time_domain_data(),
                );
                for _ in 0..2 {
                    let before = migo_alloc_probe::thread_counts();
                    node.process(&input, &mut actual, 48000, channels, 0.0);
                    let after = migo_alloc_probe::thread_counts();
                    assert_eq!(
                        (
                            after.allocations - before.allocations,
                            after.reallocations - before.reallocations
                        ),
                        (0, 0),
                        "parameter replacement made first render allocate"
                    );
                    reference.process(&input, &mut expected, 48000, channels, 0.0);
                    pcm_eq(&actual, &expected);
                    pcm_eq(
                        &node.get_float_time_domain_data(),
                        &reference.get_float_time_domain_data(),
                    );
                    assert_eq!(
                        node.get_byte_frequency_data(),
                        reference.get_byte_frequency_data(),
                        "FFT table replacement changed frequency bins"
                    );
                    pcm_eq(
                        &node.get_float_frequency_data(),
                        &reference.get_float_frequency_data(),
                    );
                }
            })
            .unwrap();
            drop(filler);
        }
        // Final close remains allowed after the original CPU host has closed.
        host.close();
        ctx.close();
        wait(|| ledger.used_bytes() == 0);
    }
}

pub(crate) fn oversampling_tables() {
    for channels in [1, 2, 32] {
        let host = migo_io::scheduler::IoScheduler::new(1);
        let cpu = host.audio_cpu();
        let ledger = Arc::new(AudioAggregateLedger::new(LIMIT));
        let usage = Arc::new(PcmUsage::new(16 << 20, 16));
        let process = Arc::new(PcmUsage::new(16 << 20, 16));
        let mut ctx = context(&cpu, &ledger, &usage, &process, channels);
        let mut original = WaveShaperNode::new(10);
        let mut reference = WaveShaperNode::new(11);
        original.set_curve(Some(vec![-0.6, -0.4, 0.1, 0.7]));
        reference.set_curve(Some(vec![-0.6, -0.4, 0.1, 0.7]));
        assert!(ctx.add_node(Box::new(original)));
        let input: Vec<f32> = (0..128 * channels)
            .map(|i| (i % 37) as f32 / 37.0 - 0.5)
            .collect();
        let mut actual = vec![0.0; input.len()];
        let mut expected = actual.clone();
        use OversampleType::{FourX, None, TwoX};
        let mut history_ready = false;
        for (mode, deny) in [
            (TwoX, false),
            (FourX, false),
            (FourX, false),
            (None, false),
            (TwoX, false),
            (FourX, true),
        ] {
            let filler = deny.then(|| {
                ledger
                    .try_reserve(LIMIT - ledger.used_bytes(), "test FIR table quota")
                    .unwrap()
            });
            let state = Arc::new(Dropped::default());
            let gate = block(&cpu);
            let credit = ledger.used_bytes();
            let accepted = crate::node_retirement::observe_replacement(witness(&state), || {
                ctx.set_wave_shaper_oversample(10, mode)
            });
            let held = ledger.used_bytes();
            let queued = state.count.load(Ordering::Acquire);
            gate.open();
            assert!(accepted, "bounded FIR tables acquired artificial admission");
            assert_eq!(queued, 0, "oversampling tables escaped CPU retirement");
            if !history_ready && mode != OversampleType::None {
                let bytes = 2 * channels as usize * (32 * 4 + std::mem::size_of::<usize>());
                assert!(
                    held >= credit + bytes + 4096,
                    "first FIR history omitted physical credit"
                );
                history_ready = true;
            } else {
                assert_eq!(
                    held, credit,
                    "prepared FIR history acquired duplicate credit"
                );
            }
            assert_eq!(
                state.count.load(Ordering::Acquire),
                0,
                "bounded FIR tables scheduled unnecessary retirement"
            );
            if accepted {
                reference.set_oversample(mode);
            }
            ctx.with_node_typed::<WaveShaperNode, _>(10, |node| {
                for _ in 0..3 {
                    let before = migo_alloc_probe::thread_counts();
                    node.process(&input, &mut actual, 48000, channels, 0.0);
                    let after = migo_alloc_probe::thread_counts();
                    assert_eq!(
                        (
                            after.allocations - before.allocations,
                            after.reallocations - before.reallocations
                        ),
                        (0, 0),
                        "parameter replacement made first render allocate"
                    );
                    reference.process(&input, &mut expected, 48000, channels, 0.0);
                    pcm_eq(&actual, &expected);
                }
            })
            .unwrap();
            drop(filler);
        }
        host.close();
        ctx.close();
        wait(|| ledger.used_bytes() == 0);
    }
}
