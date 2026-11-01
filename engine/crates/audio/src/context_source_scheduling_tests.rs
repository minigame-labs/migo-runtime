//! Scheduled sources must gate individual frames, not whole render quanta.
use super::*;
use crate::nodes::{ConstantSourceNode, OscillatorNode};
use migo_alloc_probe::thread_counts;

fn process(
    node: &mut dyn AudioNodeProcessor,
    output: &mut [f32],
    rate: u32,
    channels: u32,
    time: f64,
) -> usize {
    let probe = thread_counts();
    std::hint::black_box(vec![0_u8; 64]);
    assert!(thread_counts().allocations > probe.allocations);
    let before = thread_counts();
    let frames = node.process(&[], output, rate, channels, time);
    let after = thread_counts();
    assert_eq!(
        after.allocations - before.allocations + after.reallocations - before.reallocations,
        0,
        "scheduled source render allocated"
    );
    frames
}

pub(super) fn constant_and_clock() {
    for rate in [44_100, 48_000] {
        for origin in [0, 128, 8192, 65537] {
            let base = origin as f64 / rate as f64;
            let clock = crate::render_time::RenderTime::new(base, rate);
            for frame in 0..=257 {
                assert_eq!(
                    clock.at(frame),
                    (origin + frame) as f64 / rate as f64,
                    "sample clock changed across block boundaries"
                );
            }
            for event in [1, 17, 31, 93, 127] {
                let mut param = crate::param::AudioParamTimeline::new(0.25, -1.0, 1.0);
                param.set_value_at_time(0.75, (origin + event) as f64 / rate as f64);
                let mut values = [0.0; 128];
                param.compute_values(base, &mut values, rate);
                assert!(values[..event].iter().all(|v| *v == 0.25));
                assert!(
                    values[event..].iter().all(|v| *v == 0.75),
                    "automation event missed its exact sample"
                );
            }
            for channels in [1, 2, 32] {
                for (start, stop) in [
                    (17.0, 97.0),
                    (0.0, 128.0),
                    (17.5, 97.25),
                    (127.5, 259.25),
                    (96.0, 32.0),
                ] {
                    let start = (origin as f64 + start) / rate as f64;
                    let stop = (origin as f64 + stop) / rate as f64;
                    let mut node = ConstantSourceNode::new(10);
                    let param = node.get_param_mut("offset").unwrap();
                    param.set_value_at_time(0.25, base);
                    param.linear_ramp_to_value_at_time(0.75, (origin + 512) as f64 / rate as f64);
                    node.start(start);
                    node.stop(stop);
                    let mut output = vec![0.375; 128 * channels as usize + channels as usize - 1];
                    for block in 0..4 {
                        output.fill(0.375);
                        let time = (origin + block * 128) as f64 / rate as f64;
                        let produced = process(&mut node, &mut output, rate, channels, time);
                        let mut expected_frames = 0;
                        for (frame, samples) in output[..128 * channels as usize]
                            .chunks_exact(channels as usize)
                            .enumerate()
                        {
                            let t = (origin + block * 128 + frame) as f64 / rate as f64;
                            let value = if t >= start && t < stop {
                                expected_frames += 1;
                                0.25 + 0.5
                                    * ((t - base) / ((origin + 512) as f64 / rate as f64 - base))
                                        as f32
                            } else {
                                0.0
                            };
                            assert!(
                                samples.iter().all(|v| v.to_bits() == value.to_bits()),
                                "constant scheduled PCM changed: rate {rate} origin {origin} block {block} frame {frame}"
                            );
                        }
                        assert_eq!(
                            produced, expected_frames,
                            "constant active frame count changed"
                        );
                        assert!(
                            output[128 * channels as usize..]
                                .iter()
                                .all(|v| *v == 0.375)
                        );
                        assert_eq!(
                            node.is_finished(),
                            stop <= (origin + (block + 1) * 128) as f64 / rate as f64,
                            "constant stop fence changed"
                        );
                    }
                    node.stop(1000.0);
                    node.start(1000.0);
                    output.fill(0.375);
                    assert_eq!(
                        process(
                            &mut node,
                            &mut output,
                            rate,
                            channels,
                            (origin + 512) as f64 / rate as f64
                        ),
                        0
                    );
                    assert!(
                        output[..128 * channels as usize].iter().all(|v| *v == 0.0),
                        "finished constant source restarted"
                    );
                }
            }
        }
        let fractional = 1.125 / rate as f64;
        let clock = crate::render_time::RenderTime::new(fractional, rate);
        assert_eq!(
            clock.at(0),
            fractional,
            "fractional caller time was quantized"
        );
    }
}

pub(super) fn oscillator_and_buffer() {
    for rate in [44_100, 48_000] {
        for channels in [1, 2, 32] {
            for (start_frame, stop_frame) in [
                (17.0, 97.0),
                (0.0, 128.0),
                (17.5, 97.25),
                (127.5, 259.25),
                (96.0, 32.0),
            ] {
                let start = start_frame / rate as f64;
                let stop = stop_frame / rate as f64;
                let mut oscillator = OscillatorNode::new(10, rate);
                oscillator
                    .get_param_mut("frequency")
                    .unwrap()
                    .set_value(333.0);
                oscillator.start(start);
                oscillator.stop(stop);
                let context = AudioContext::new(1, rate, channels);
                let mut context = context;
                let buffer = context
                    .add_buffer(DecodedAudio {
                        sample_rate: rate,
                        channels: 1,
                        samples: (0..2048).map(|n| n as f32 / 4096.0).collect(),
                    })
                    .unwrap();
                let mut source = BufferSourceNode::new(11, rate);
                source.set_buffer(context.get_buffer(buffer));
                source.start(start, 3.25 / rate as f64, None, 0.0);
                source.stop(stop);
                let mut a = vec![0.375; 128 * channels as usize + channels as usize - 1];
                let mut b = a.clone();
                let mut phase = None;
                let mut position = None;
                for block in 0..4 {
                    a.fill(0.375);
                    b.fill(0.375);
                    let current = (block * 128) as f64 / rate as f64;
                    let na = process(&mut oscillator, &mut a, rate, channels, current);
                    let nb = process(&mut source, &mut b, rate, channels, current);
                    let mut generated = 0;
                    for frame in 0..128 {
                        let time = (block * 128 + frame) as f64 / rate as f64;
                        let (sine, ramp) = if time >= start && time < stop {
                            generated += 1;
                            let phase = phase.get_or_insert(((time - start) * 333.0).fract());
                            let sine = (*phase * std::f64::consts::TAU).sin() as f32;
                            *phase = (*phase + 333.0 / rate as f64).fract();
                            let cursor = position.get_or_insert(
                                (3.25 / rate as f64) * rate as f64 + (time - start) * rate as f64,
                            );
                            let ramp = (*cursor / 4096.0) as f32;
                            *cursor += 1.0;
                            (sine, ramp)
                        } else {
                            (0.0, 0.0)
                        };
                        let from = frame * channels as usize;
                        let to = from + channels as usize;
                        assert!(
                            a[from..to].iter().all(|v| v.to_bits() == sine.to_bits()),
                            "oscillator scheduled PCM or initial phase changed"
                        );
                        assert!(
                            b[from..to].iter().all(|v| v.to_bits() == ramp.to_bits()),
                            "buffer source scheduled PCM or fractional offset changed"
                        );
                    }
                    assert_eq!(na, generated, "oscillator active frame count changed");
                    assert_eq!(nb, generated, "buffer active frame count changed");
                    assert!(
                        a[128 * channels as usize..]
                            .iter()
                            .chain(&b[128 * channels as usize..])
                            .all(|v| *v == 0.375)
                    );
                    let finished = stop <= (block + 1) as f64 * 128.0 / rate as f64;
                    assert_eq!(
                        oscillator.is_finished(),
                        finished,
                        "oscillator stop fence changed"
                    );
                    assert_eq!(source.is_finished(), finished, "buffer stop fence changed");
                }
                oscillator.stop(1000.0);
                source.stop(1000.0);
                a.fill(0.375);
                b.fill(0.375);
                assert_eq!(
                    process(&mut oscillator, &mut a, rate, channels, 512.0 / rate as f64),
                    0
                );
                assert_eq!(
                    process(&mut source, &mut b, rate, channels, 512.0 / rate as f64),
                    0
                );
                assert!(
                    a[..128 * channels as usize]
                        .iter()
                        .chain(&b[..128 * channels as usize])
                        .all(|v| *v == 0.0),
                    "finished scheduled source restarted"
                );
            }
        }
        let mut empty = BufferSourceNode::new(10, rate);
        empty.start(0.0, 0.0, None, 0.0);
        empty.stop(17.0 / rate as f64);
        assert_eq!(empty.process(&[], &mut [0.0; 128], rate, 1, 0.0), 0);
        assert!(empty.is_finished(), "unbound buffer ignored its stop time");
    }
    let mut late = OscillatorNode::new(10, 48000);
    late.start(0.0);
    let mut sample = [0.375];
    late.process(&[], &mut sample, 48000, 1, 128.0 / 48000.0);
    assert_eq!(
        sample[0], 0.0,
        "past oscillator start did not start immediately"
    );
}
