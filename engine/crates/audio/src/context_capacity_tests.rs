//! Warmup hid topology scratch growth beyond DEFAULT_NODE_CAPACITY. Exercise
//! the first quantum after commands, not a previously compiled graph.
use super::*;
use migo_alloc_probe::thread_counts;

const VOICES: u32 = 32;
const MASTER: u32 = 5;

fn render(context: &mut AudioContext, output: &mut [f32]) -> usize {
    // Prove this thread's allocator sees real allocations without rendering or
    // otherwise warming the graph under test.
    let probe_start = thread_counts();
    std::hint::black_box(vec![0_u8; 64]);
    assert!(thread_counts().allocations > probe_start.allocations);
    let before = thread_counts();
    let collected = context.process(output).len();
    let after = thread_counts();
    assert_eq!(
        (after.allocations - before.allocations) + (after.reallocations - before.reallocations),
        0,
        "cold context render allocated"
    );
    collected
}

fn voices(context: &mut AudioContext, buffer: u32, first: u32) {
    let when = context.current_time();
    for voice in first..first + VOICES {
        let id = 10 + voice * 2;
        context.create_buffer_source(id);
        context.set_buffer(id, Some(buffer));
        context.watch_source_ended(id, true);
        context.create_gain(id + 1);
        context.set_gain(id + 1, 1.0 / VOICES as f32);
        context.connect(id, id + 1);
        context.connect(id + 1, MASTER);
        context.start_source(id, when, 0.0, None);
    }
}

fn graph(frames: usize) -> (AudioContext, u32, Vec<f32>) {
    let mut context = AudioContext::new(1, 48_000, 2);
    let pcm: Vec<_> = (0..frames * 2)
        .map(|i| ((i % 31) as i32 - 15) as f32 / 512.0)
        .collect();
    let buffer = context
        .add_buffer(DecodedAudio {
            sample_rate: 48_000,
            channels: 2,
            samples: pcm.clone(),
        })
        .unwrap();
    context.create_gain(MASTER);
    context.set_gain(MASTER, 0.5);
    context.connect(MASTER, DESTINATION_NODE_ID);
    voices(&mut context, buffer, 0);
    (context, buffer, pcm)
}

pub(super) fn cold_graph_and_collection() {
    let (mut context, _, pcm) = graph(128);
    let mut output = [0.125; 256];
    assert_eq!(
        render(&mut context, &mut output),
        VOICES as usize,
        "cold graph sources were not collected in their final block"
    );
    for (sample, expected) in output.iter().zip(pcm) {
        assert_eq!(
            sample.to_bits(),
            (0.125 + expected * 0.5).to_bits(),
            "cold graph PCM changed"
        );
    }
    let mut ended: Vec<_> = context.drain_ended().collect();
    ended.sort_unstable();
    assert_eq!(ended, (0..VOICES).map(|n| 10 + n * 2).collect::<Vec<_>>());
    output.fill(0.125);
    assert_eq!(render(&mut context, &mut output), 0);
    assert_eq!(output, [0.125; 256], "finished cold source replayed PCM");
    assert!(context.drain_ended().next().is_none());
    output.fill(0.125);
    assert_eq!(render(&mut context, &mut output), 0);
    assert_eq!(output, [0.125; 256], "collected cold graph replayed PCM");
    assert!(context.drain_ended().next().is_none());
}

pub(super) fn growth_after_first_render() {
    let (mut context, buffer, _) = graph(1024);
    let mut output = [0.0; 256];
    render(&mut context, &mut output);
    voices(&mut context, buffer, VOICES);
    output.fill(0.0);
    render(&mut context, &mut output);
    assert!(output.iter().all(|v| v.is_finite()));
    assert!(
        output.iter().any(|v| *v != 0.0),
        "grown graph did not produce PCM"
    );
}

pub(super) fn edge_growth_without_new_nodes() {
    let mut context = AudioContext::new(1, 48_000, 2);
    let buffer = context
        .add_buffer(DecodedAudio {
            sample_rate: 48_000,
            channels: 2,
            samples: vec![1.0 / 1024.0; 1024],
        })
        .unwrap();
    context.create_buffer_source(1);
    context.set_buffer(1, Some(buffer));
    context.start_source(1, 0.0, 0.0, None);
    for id in 100..124 {
        context.create_gain(id);
    }
    context.connect(1, 100);
    context.connect(100, DESTINATION_NODE_ID);
    let mut output = [0.0; 256];
    render(&mut context, &mut output);
    for id in 100..124 {
        context.connect(1, id);
        context.connect(id, DESTINATION_NODE_ID);
    }
    for from in 100..124 {
        for to in from + 1..124 {
            context.connect(from, to);
        }
    }
    output.fill(0.0);
    render(&mut context, &mut output);
    assert!(output.iter().all(|v| v.is_finite()));
    assert!(output.iter().all(|v| *v > 0.0), "fan-in graph lost PCM");
}

pub(super) fn first_automation() {
    for rate in [44_100, 48_000] {
        for late in [false, true] {
            let mut context = AudioContext::new(1, rate, 2);
            let buffer = context
                .add_buffer(DecodedAudio {
                    sample_rate: rate,
                    channels: 2,
                    samples: vec![0.25; 2048],
                })
                .unwrap();
            context.create_buffer_source(10);
            context.set_buffer(10, Some(buffer));
            context.create_gain(11);
            context.set_gain(11, 0.5);
            context.connect(10, 11);
            context.connect(11, DESTINATION_NODE_ID);
            context.start_source(10, 0.0, 0.0, None);
            let mut output = [0.125; 256];
            if late {
                render(&mut context, &mut output);
                assert_eq!(output, [0.25; 256]);
                output.fill(0.125);
            }
            let now = context.current_time();
            assert!(context.param_set_value_at_time(11, "gain", 0.25, now));
            assert!(context.param_linear_ramp(11, "gain", 0.75, now + 128.0 / rate as f64));
            render(&mut context, &mut output);
            for (frame, samples) in output.chunks_exact(2).enumerate() {
                let expected = 0.125 + 0.25 * (0.25 + 0.5 * frame as f32 / 128.0);
                assert!(
                    samples.iter().all(|v| v.to_bits() == expected.to_bits()),
                    "automated gain PCM changed"
                );
            }
        }
    }
    use crate::nodes::DelayNode;
    for rate in [44_100, 48_000] {
        for channels in [1, 2, 32] {
            for frames in [1, 17, 128] {
                let mut node = DelayNode::new(1, 0.001, rate, channels);
                let seconds = 0.5 / rate as f32;
                node.get_param_mut("delayTime")
                    .unwrap()
                    .set_value_at_time(seconds, 0.0);
                let fraction = seconds * rate as f32;
                let mut input = vec![0.0; frames * channels as usize];
                for (ch, value) in input.iter_mut().take(channels as usize).enumerate() {
                    *value = 2.0_f32.powi(-((ch % 8) as i32));
                }
                let mut output = vec![0.375; input.len() + channels as usize - 1];
                node_render(&mut node, &input, &mut output, rate, channels, 0.0);
                for (frame, samples) in output[..input.len()]
                    .chunks_exact(channels as usize)
                    .enumerate()
                {
                    for (ch, value) in samples.iter().enumerate() {
                        let impulse = input[ch];
                        let expected = if frame == 0 {
                            impulse * (1.0 - fraction)
                        } else if frame == 1 {
                            impulse * fraction
                        } else {
                            0.0
                        };
                        assert_eq!(
                            value.to_bits(),
                            expected.to_bits(),
                            "automated delay PCM changed"
                        );
                    }
                }
                assert!(output[input.len()..].iter().all(|v| *v == 0.375));
                let mut tail = vec![0.0; 128 * channels as usize];
                node_render(
                    &mut node,
                    &[],
                    &mut tail,
                    rate,
                    channels,
                    frames as f64 / rate as f64,
                );
                for (index, value) in tail.iter().enumerate() {
                    let expected = if frames == 1 && index < channels as usize {
                        input[index] * fraction
                    } else {
                        0.0
                    };
                    assert_eq!(
                        value.to_bits(),
                        expected.to_bits(),
                        "automated delay tail changed"
                    );
                }
                assert!(
                    !node.has_tail_audio(),
                    "automated delay tail did not become idle"
                );
            }
        }
    }
}

fn node_render(
    node: &mut dyn AudioNodeProcessor,
    input: &[f32],
    output: &mut [f32],
    rate: u32,
    channels: u32,
    time: f64,
) {
    let probe = thread_counts();
    std::hint::black_box(vec![0_u8; 64]);
    assert!(thread_counts().allocations > probe.allocations);
    let before = thread_counts();
    node.process(input, output, rate, channels, time);
    let after = thread_counts();
    assert_eq!(
        after.allocations - before.allocations + after.reallocations - before.reallocations,
        0,
        "cold automation node allocated"
    );
}

pub(super) fn automation_long_blocks() {
    use crate::nodes::DelayNode;
    let mut gain = GainNode::new(1);
    let param = gain.get_param_mut("gain").unwrap();
    param.set_value_at_time(0.25, 0.0);
    param.linear_ramp_to_value_at_time(0.75, 256.0 / 48_000.0);
    let input = vec![0.25; 257 * 2];
    let mut output = vec![0.0; input.len()];
    assert_eq!(gain.process(&input, &mut output, 48_000, 2, 0.0), 257);
    for (frame, samples) in output.chunks_exact(2).enumerate() {
        let expected = 0.25 * (0.25 + 0.5 * frame as f32 / 256.0);
        assert!(
            samples.iter().all(|v| v.to_bits() == expected.to_bits()),
            "long automation block was truncated or changed"
        );
    }
    let mut delay = DelayNode::new(2, 0.001, 48_000, 2);
    output.fill(0.0);
    assert_eq!(delay.process(&input, &mut output, 48_000, 2, 0.0), 257);
    assert_eq!(output, input, "long delay block was truncated or changed");
}

pub(super) fn wave_shaper_commands() {
    use crate::nodes::{
        OversampleType::{FourX, None as NoOversampling, TwoX},
        WaveShaperNode,
    };
    for rate in [44_100, 48_000] {
        for channels in [1, 2, 32] {
            for curve in [None, Some(vec![-1.0, -1.0, 0.0, 1.0, 1.0])] {
                let mut context = AudioContext::new(1, rate, channels);
                let pcm: Vec<_> = (0..2048 * channels as usize)
                    .map(|i| ((i % 31) as i32 - 15) as f32 / 16.0)
                    .collect();
                let buffer = context
                    .add_buffer(DecodedAudio {
                        sample_rate: rate,
                        channels,
                        samples: pcm.clone(),
                    })
                    .unwrap();
                context.create_buffer_source(10);
                context.set_buffer(10, Some(buffer));
                let mut node = WaveShaperNode::new(11);
                node.set_curve(curve.clone());
                context.add_node(Box::new(node));
                context.connect(10, 11);
                context.connect(11, DESTINATION_NODE_ID);
                context.start_source(10, 0.0, 0.0, None);
                let mut reference = WaveShaperNode::new(11);
                reference.set_curve(curve);
                let mut output = vec![0.125; 128 * channels as usize];
                let mut expected = vec![0.0; output.len()];
                for (block, mode) in [TwoX, TwoX, FourX, FourX, NoOversampling, TwoX]
                    .into_iter()
                    .enumerate()
                {
                    assert!(context.set_wave_shaper_oversample(11, mode));
                    // The unprepared reference retains the original process path.
                    // It is rendered only outside the allocation window.
                    reference.set_oversample(mode, 1);
                    let from = block * output.len();
                    assert_eq!(
                        reference.process(
                            &pcm[from..from + output.len()],
                            &mut expected,
                            rate,
                            channels,
                            block as f64 * 128.0 / rate as f64,
                        ),
                        128
                    );
                    output.fill(0.125);
                    render(&mut context, &mut output);
                    for (sample, reference) in output.iter().zip(&expected) {
                        assert_eq!(
                            sample.to_bits(),
                            (0.125 + reference).to_bits(),
                            "prepared waveshaper PCM or history changed"
                        );
                    }
                }
            }
        }
    }
}

pub(super) fn wave_shaper_long_blocks() {
    use crate::nodes::{
        OversampleType::{FourX, None as NoOversampling, TwoX},
        WaveShaperNode,
    };
    for channels in [1, 2, 32] {
        let mut prepared = WaveShaperNode::new(1);
        let mut reference = WaveShaperNode::new(1);
        prepared.set_curve(Some(vec![-1.0, 0.0, 1.0]));
        reference.set_curve(Some(vec![-1.0, 0.0, 1.0]));
        for mode in [TwoX, FourX, FourX, NoOversampling, TwoX] {
            prepared.set_oversample(mode, 1);
            prepared.prepare_oversampling(channels);
            reference.set_oversample(mode, 1);
            for frames in [1, 17, 128, 257] {
                let input: Vec<_> = (0..frames * channels as usize)
                    .map(|i| ((i % 31) as i32 - 15) as f32 / 16.0)
                    .collect();
                let mut output = vec![0.375; input.len() + channels as usize - 1];
                let mut expected = output.clone();
                assert_eq!(
                    prepared.process(&input, &mut output, 48_000, channels, 0.0),
                    frames
                );
                assert_eq!(
                    reference.process(&input, &mut expected, 48_000, channels, 0.0),
                    frames
                );
                assert_eq!(
                    output.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                    expected.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                    "long waveshaper block or partial frame changed"
                );
            }
        }
    }
}
