//! Finite FIR history must survive source collection; DC needs a live output.
//! Explicit zero input through the original kernel is the tail reference.
use super::*;
use crate::nodes::{OversampleType, WaveShaperNode};
use migo_alloc_probe::thread_counts;

fn node(id: u32, mode: OversampleType, channels: u32, curve: Vec<f32>) -> WaveShaperNode {
    let mut node = WaveShaperNode::new(id);
    node.set_curve(Some(curve));
    node.set_oversample(mode);
    node.prepare_oversampling(channels);
    node
}

fn device_render(context: &mut AudioContext, output: &mut [f32]) -> Vec<u32> {
    let probe = thread_counts();
    std::hint::black_box(vec![0_u8; 64]);
    assert!(thread_counts().allocations > probe.allocations);
    let before = thread_counts();
    let collected = context.process_device_source(output);
    let after = thread_counts();
    assert_eq!(
        after.allocations - before.allocations + after.reallocations - before.reallocations,
        0,
        "waveshaper lifecycle render allocated"
    );
    collected.to_vec()
}

fn same_pcm(actual: &[f32], expected: &[f32], message: &str) {
    assert_eq!(actual.len(), expected.len());
    assert!(
        actual
            .iter()
            .zip(expected)
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "{message}"
    );
}

pub(super) fn tail_and_collection() {
    for mode in [OversampleType::TwoX, OversampleType::FourX] {
        for rate in [44_100, 48_000] {
            for channels in [1, 2, 32] {
                // Single-frame empty-input turns expose the downstream FIR
                // after the upstream history has already become exactly zero.
                let curve = vec![-1.0, 0.0, 1.0];
                let mut live = node(1, mode, channels, curve.clone());
                let mut reference = node(1, mode, channels, curve.clone());
                let impulse = vec![0.5; channels as usize];
                let mut actual = vec![0.0; channels as usize];
                let mut expected = actual.clone();
                live.process(&impulse, &mut actual, rate, channels, 0.0);
                reference.process(&impulse, &mut expected, rate, channels, 0.0);
                let zero = vec![0.0; channels as usize];
                let mut audible = false;
                for _ in 0..64 {
                    actual.fill(0.0);
                    reference.process(&zero, &mut expected, rate, channels, 0.0);
                    let before = thread_counts();
                    let frames = live.process(&[], &mut actual, rate, channels, 0.0);
                    let after = thread_counts();
                    assert_eq!(
                        after.allocations - before.allocations + after.reallocations
                            - before.reallocations,
                        0,
                        "waveshaper empty tail allocated"
                    );
                    assert!(frames <= 1);
                    audible |= expected.iter().any(|&v| v != 0.0);
                    same_pcm(&actual, &expected, "waveshaper FIR tail PCM changed");
                }
                assert!(audible, "FIR tail reference was silent");
                assert!(!live.has_tail_audio(), "finite FIR tail never became idle");

                for frames in [112, 120, 127, 255] {
                    let ch = channels as usize;
                    let mut context = AudioContext::new(1, rate, channels);
                    let mut pcm = vec![0.0; frames * ch];
                    pcm[(frames - 1) * ch..].fill(0.5);
                    let buffer = context
                        .add_buffer(DecodedAudio {
                            sample_rate: rate,
                            channels,
                            samples: pcm.clone(),
                        })
                        .unwrap();
                    context.create_buffer_source(10);
                    context.set_buffer(10, Some(buffer));
                    context.watch_source_ended(10, true);
                    context.add_node(Box::new(node(11, mode, channels, curve.clone())));
                    context.create_gain(12);
                    context.connect(10, 11);
                    context.connect(11, 12);
                    context.connect(12, DESTINATION_NODE_ID);
                    context.start_source(10, 0.0, 0.0, None);
                    for id in [12, 11, 10] {
                        assert!(context.release_node(id).is_empty());
                    }
                    let blocks = frames.div_ceil(128) + 1;
                    pcm.resize(blocks * 128 * ch, 0.0);
                    let mut reference = node(1, mode, channels, curve.clone());
                    let mut output = vec![0.375; 128 * ch];
                    let mut expected = vec![0.0; output.len()];
                    let mut removed = Vec::new();
                    for (block, input) in pcm.chunks(128 * ch).enumerate() {
                        reference.process(input, &mut expected, rate, channels, 0.0);
                        if block + 1 == blocks && expected.iter().any(|&v| v != 0.0) {
                            assert!(
                                context.has_active_sources(),
                                "waveshaper tail graph became idle early"
                            );
                            assert!(
                                context.nodes.contains_key(&11),
                                "released shaper lost its FIR tail"
                            );
                            context.suspend();
                            output.fill(0.375);
                            let frame = context.frames_processed;
                            assert!(device_render(&mut context, &mut output).is_empty());
                            assert!(output.iter().all(|v| *v == 0.375));
                            assert_eq!(context.frames_processed, frame);
                            context.resume();
                        }
                        removed.extend(device_render(&mut context, &mut output));
                        same_pcm(
                            &output,
                            &expected,
                            "released waveshaper graph tail PCM changed",
                        );
                    }
                    removed.sort_unstable();
                    assert_eq!(removed, [10, 11, 12], "waveshaper tail chain leaked nodes");
                    assert_eq!(context.drain_ended().collect::<Vec<_>>(), [10]);
                    assert!(
                        !context.has_active_sources(),
                        "drained waveshaper kept graph active"
                    );
                    output.fill(0.375);
                    assert!(device_render(&mut context, &mut output).is_empty());
                    assert!(
                        output.iter().all(|v| *v == 0.0),
                        "idle shaper replayed a tail"
                    );
                }
            }
        }
    }
}

pub(super) fn dc_and_connections() {
    for mode in [
        OversampleType::None,
        OversampleType::TwoX,
        OversampleType::FourX,
    ] {
        for channels in [1, 2, 32] {
            // Odd/even curves and a constant curve all map zero to 0.25.
            for curve in [vec![-0.5, 0.25, 1.0], vec![-0.5, 1.0], vec![0.25, 0.25]] {
                let mut context = AudioContext::new(1, 48_000, channels);
                context.add_node(Box::new(node(11, mode, channels, curve.clone())));
                context.create_gain(12);
                context.create_gain(13);
                assert!(
                    !context.has_active_sources(),
                    "unconnected DC shaper kept graph active"
                );
                context.connect(11, 12);
                context.connect(12, DESTINATION_NODE_ID);
                context.connect(11, 12); // Duplicate connections are idempotent.
                assert!(
                    context.has_active_sources(),
                    "connected DC waveshaper did not become active"
                );
                let zero = vec![0.0; 128 * channels as usize];
                let mut reference = node(1, mode, channels, curve);
                let mut expected = zero.clone();
                let mut output = zero.clone();
                for _ in 0..2 {
                    reference.process(&zero, &mut expected, 48_000, channels, 0.0);
                    assert!(device_render(&mut context, &mut output).is_empty());
                    same_pcm(&output, &expected, "waveshaper DC PCM changed");
                }
                assert!(
                    !reference.has_tail_audio(),
                    "steady DC was mistaken for a finite tail"
                );
                context.disconnect(11, None);
                assert!(
                    !context.has_active_sources(),
                    "disconnected DC waveshaper kept graph active"
                );
                device_render(&mut context, &mut output);
                assert!(output.iter().all(|v| *v == 0.0));
                context.connect(11, 12);
                reference.process(&zero, &mut expected, 48_000, channels, 0.0);
                device_render(&mut context, &mut output);
                same_pcm(
                    &output,
                    &expected,
                    "reconnected waveshaper DC history changed",
                );
                context.connect(11, 13);
                context.disconnect(11, Some(12));
                assert!(
                    context.has_active_sources(),
                    "partial disconnect lost DC activity"
                );
                context.disconnect(11, Some(13));
                assert!(
                    !context.has_active_sources(),
                    "last DC output disconnect was ignored"
                );
                assert_eq!(context.release_node(11), [11]);
                assert!(context.drain_ended().next().is_none());
            }
        }
    }
    // A replacement keeps its existing graph edges, even before it renders.
    let mut context = AudioContext::new(1, 48_000, 2);
    context.add_node(Box::new(WaveShaperNode::new(11)));
    context.connect(11, DESTINATION_NODE_ID);
    context.add_node(Box::new(node(
        11,
        OversampleType::None,
        2,
        vec![0.25, 0.25],
    )));
    assert!(
        context.has_active_sources(),
        "replacement shaper lost output connection"
    );

    struct FinishedSink;
    impl AudioNodeProcessor for FinishedSink {
        fn id(&self) -> u32 {
            12
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn process(&mut self, _: &[f32], _: &mut [f32], _: u32, _: u32, _: f64) -> usize {
            0
        }
        fn is_finished(&self) -> bool {
            true
        }
    }
    context.disconnect(11, None);
    context.add_node(Box::new(FinishedSink));
    context.connect(11, 12);
    let mut output = [0.0; 256];
    assert_eq!(device_render(&mut context, &mut output), [12]);
    assert!(
        !context.has_active_sources(),
        "pruned last output kept DC active"
    );
}
