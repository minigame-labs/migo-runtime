//! Source AudioParams were evaluated once per quantum despite being a-rate.
//! Compare every frame with analytic curves and the unchanged scalar oscillator.
use super::*;
use crate::nodes::{ConstantSourceNode, OscillatorNode, OscillatorType};
use migo_alloc_probe::thread_counts;

fn render(context: &mut AudioContext, output: &mut [f32]) {
    let probe = thread_counts();
    std::hint::black_box(vec![0_u8; 64]);
    assert!(thread_counts().allocations > probe.allocations);
    let before = thread_counts();
    assert!(context.process(output).is_empty());
    let after = thread_counts();
    assert_eq!(
        after.allocations - before.allocations + after.reallocations - before.reallocations,
        0,
        "source automation render allocated"
    );
}

pub(super) fn offset() {
    let mut timeline = crate::param::AudioParamTimeline::new(0.0, -1.0, 1.0);
    timeline.set_value_at_time(0.25, 1.0);
    timeline.set_value_at_time(0.75, 1.0);
    assert_eq!(
        timeline.compute_value(1.0),
        0.75,
        "equal-time events lost call order"
    );
    for rate in [44_100, 48_000] {
        for channels in [1, 2, 32] {
            for late in [false, true] {
                for curve in 0..4 {
                    let mut context = AudioContext::new(1, rate, channels);
                    let mut source = ConstantSourceNode::new(10);
                    source.get_param_mut("offset").unwrap().set_value(0.25);
                    source.start(0.0);
                    context.add_node(Box::new(source));
                    context.connect(10, DESTINATION_NODE_ID);
                    let mut output = vec![0.125; 128 * channels as usize];
                    if late {
                        render(&mut context, &mut output);
                    }
                    let start = context.current_time();
                    let end = start + 128.0 / rate as f64;
                    context.param_set_value_at_time(10, "offset", 0.25, start);
                    match curve {
                        0 => {
                            context.param_linear_ramp(10, "offset", 0.75, end);
                        }
                        1 => {
                            context.param_exponential_ramp(10, "offset", 1.0, end);
                        }
                        2 => {
                            context.param_set_target(10, "offset", 0.75, start, 64.0 / rate as f64);
                        }
                        _ => {
                            context.param_set_value_at_time(
                                10,
                                "offset",
                                0.75,
                                start + 64.0 / rate as f64,
                            );
                        }
                    }
                    output.fill(0.125);
                    render(&mut context, &mut output);
                    for (frame, samples) in output.chunks_exact(channels as usize).enumerate() {
                        let time = start + frame as f64 * (1.0 / rate as f64);
                        let x = (time - start) / (end - start);
                        let value = match curve {
                            0 => 0.25 + 0.5 * x as f32,
                            1 => 0.25 * 4.0_f32.powf(x as f32),
                            2 => {
                                0.75 - 0.5 * ((-(time - start) / (64.0 / rate as f64)).exp() as f32)
                            }
                            _ => {
                                if time < start + 64.0 / rate as f64 {
                                    0.25
                                } else {
                                    0.75
                                }
                            }
                        };
                        assert!(
                            samples
                                .iter()
                                .all(|v| v.to_bits() == (0.125 + value).to_bits()),
                            "constant offset a-rate PCM changed: rate {rate}, channels {channels}, late {late}, curve {curve}, frame {frame}, actual {}, expected {}",
                            samples[0],
                            0.125 + value
                        );
                    }
                }
            }
        }
    }
    // Arbitrary direct calls retain their >128-frame resizing fallback.
    let mut source = ConstantSourceNode::new(10);
    let param = source.get_param_mut("offset").unwrap();
    param.set_value_at_time(0.25, 0.0);
    param.linear_ramp_to_value_at_time(0.75, 256.0 / 48_000.0);
    source.start(0.0);
    let mut output = vec![0.375; 257 * 2 + 1];
    assert_eq!(source.process(&[], &mut output, 48_000, 2, 0.0), 257);
    for (frame, values) in output[..514].chunks_exact(2).enumerate() {
        let expected = 0.25 + 0.5 * frame as f32 / 256.0;
        assert!(
            values.iter().all(|v| v.to_bits() == expected.to_bits()),
            "long constant automation changed"
        );
    }
    assert_eq!(output[514], 0.375);
}

pub(super) fn oscillator() {
    for rate in [44_100, 48_000] {
        for channels in [1, 2, 32] {
            for late in [false, true] {
                for mode in [
                    OscillatorType::Sine,
                    OscillatorType::Square,
                    OscillatorType::Sawtooth,
                    OscillatorType::Triangle,
                ] {
                    for mask in 1..=3 {
                        let mut context = AudioContext::new(1, rate, channels);
                        let mut source = OscillatorNode::new(10, rate);
                        source.set_type(mode);
                        source.get_param_mut("frequency").unwrap().set_value(333.0);
                        source.start(0.0);
                        context.add_node(Box::new(source));
                        context.connect(10, DESTINATION_NODE_ID);
                        let mut reference = OscillatorNode::new(10, rate);
                        reference.set_type(mode);
                        reference
                            .get_param_mut("frequency")
                            .unwrap()
                            .set_value(333.0);
                        reference.start(0.0);
                        let mut output = vec![0.125; 128 * channels as usize];
                        if late {
                            render(&mut context, &mut output);
                            let mut warm = vec![0.0; output.len()];
                            reference.process(&[], &mut warm, rate, channels, 0.0);
                        }
                        let start = context.current_time();
                        let end = start + 128.0 / rate as f64;
                        if mask & 1 != 0 {
                            context.param_set_value_at_time(10, "frequency", 333.0, start);
                            context.param_linear_ramp(10, "frequency", 777.0, end);
                        }
                        if mask & 2 != 0 {
                            context.param_set_value_at_time(10, "detune", 0.0, start);
                            context.param_linear_ramp(10, "detune", 1200.0, end);
                        }
                        output.fill(0.125);
                        render(&mut context, &mut output);
                        let mut expected = vec![0.0; channels as usize];
                        for (frame, samples) in output.chunks_exact(channels as usize).enumerate() {
                            let time = start + frame as f64 * (1.0 / rate as f64);
                            let x = (time - start) / (end - start);
                            let frequency = if mask & 1 != 0 {
                                333.0 + 444.0 * x as f32
                            } else {
                                333.0
                            };
                            let detune = if mask & 2 != 0 {
                                1200.0 * x as f32
                            } else {
                                0.0
                            };
                            reference
                                .get_param_mut("frequency")
                                .unwrap()
                                .set_value(frequency);
                            reference.get_param_mut("detune").unwrap().set_value(detune);
                            assert_eq!(
                                reference.process(&[], &mut expected, rate, channels, time),
                                1
                            );
                            assert!(
                                samples
                                    .iter()
                                    .zip(&expected)
                                    .all(|(a, b)| a.to_bits() == (0.125 + b).to_bits()),
                                "oscillator a-rate PCM or phase changed"
                            );
                        }
                    }
                }
            }
        }
    }
    long_oscillator();
}

fn long_oscillator() {
    let mut source = OscillatorNode::new(10, 48_000);
    source
        .get_param_mut("frequency")
        .unwrap()
        .set_value_at_time(333.0, 0.0);
    source
        .get_param_mut("frequency")
        .unwrap()
        .linear_ramp_to_value_at_time(777.0, 256.0 / 48_000.0);
    source
        .get_param_mut("detune")
        .unwrap()
        .set_value_at_time(0.0, 0.0);
    source
        .get_param_mut("detune")
        .unwrap()
        .linear_ramp_to_value_at_time(1200.0, 256.0 / 48_000.0);
    source.start(0.0);
    let mut output = vec![0.375; 257 * 2 + 1];
    assert_eq!(source.process(&[], &mut output, 48_000, 2, 0.0), 257);
    let mut reference = OscillatorNode::new(10, 48_000);
    reference.start(0.0);
    for (frame, actual) in output[..514].chunks_exact(2).enumerate() {
        let time = frame as f64 * (1.0 / 48_000.0);
        let x = (time / (256.0 / 48_000.0)) as f32;
        reference
            .get_param_mut("frequency")
            .unwrap()
            .set_value(333.0 + 444.0 * x);
        reference
            .get_param_mut("detune")
            .unwrap()
            .set_value(1200.0 * x);
        let mut expected = [0.0; 2];
        reference.process(&[], &mut expected, 48_000, 2, time);
        assert_eq!(actual, expected, "long oscillator automation changed");
    }
    assert_eq!(output[514], 0.375);
}
