//! Full old kernels and the old timeline independently guard stack chunking.
use crate::nodes::*;
use crate::param::AudioParamTimeline;
use migo_alloc_probe::thread_counts;
#[path = "nodes/inline_automation_constant_source_reference.rs"]
mod constant_before;
#[path = "nodes/inline_automation_delay_reference.rs"]
mod delay_before;
#[path = "nodes/inline_automation_gain_reference.rs"]
mod gain_before;
#[path = "nodes/inline_automation_oscillator_reference.rs"]
mod oscillator_before;
#[path = "nodes/inline_automation_param_reference.rs"]
mod param_before;

fn same(a: &[f32], b: &[f32], message: &str) {
    assert!(
        a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.to_bits() == b.to_bits()),
        "{message}"
    );
}
fn cold_process(
    node: &mut dyn AudioNodeProcessor,
    input: &[f32],
    output: &mut [f32],
    rate: u32,
    ch: u32,
    time: f64,
) -> usize {
    let before = thread_counts();
    let frames = node.process(input, output, rate, ch, time);
    let after = thread_counts();
    assert_eq!(
        (
            after.allocations - before.allocations,
            after.reallocations - before.reallocations
        ),
        (0, 0),
        "bounded automation render allocated"
    );
    frames
}
fn schedule(param: &mut AudioParamTimeline, time: f64, rate: u32, kind: u32) {
    param.set_value_at_time(0.25, time);
    match kind {
        0 => param.linear_ramp_to_value_at_time(0.875, time + 8192.0 / rate.max(1) as f64),
        1 => param.exponential_ramp_to_value_at_time(0.875, time + 8192.0 / rate.max(1) as f64),
        _ => param.set_target_at_time(0.875, time + 127.0 / rate.max(1) as f64, 0.001),
    }
}
#[test]
fn constructors_and_parameter_access_have_no_scratch_heap() {
    let before = thread_counts();
    let mut gain = GainNode::new(10);
    gain.get_param_mut("gain").unwrap();
    assert_eq!(
        thread_counts().allocations - before.allocations,
        0,
        "gain command retained scratch heap"
    );
    let before = thread_counts();
    let mut source = ConstantSourceNode::new(11);
    source.get_param_mut("offset").unwrap();
    assert_eq!(
        thread_counts().allocations - before.allocations,
        0,
        "constant command retained scratch heap"
    );
    let before = thread_counts();
    let mut oscillator = OscillatorNode::new(12, 48000);
    oscillator.get_param_mut("frequency").unwrap();
    oscillator.get_param_mut("detune").unwrap();
    assert_eq!(
        thread_counts().allocations - before.allocations,
        0,
        "oscillator command retained scratch heap"
    );
    let ledger = std::sync::Arc::new(crate::limits::AudioAggregateLedger::new(16 << 20));
    let before = thread_counts();
    let mut delay = DelayNode::new_with_aggregate(13, 0.001, 48000, 2, &ledger).unwrap();
    delay.get_param_mut("delayTime").unwrap();
    assert_eq!(
        thread_counts().allocations - before.allocations,
        1,
        "delay retained scratch beyond circular buffer"
    );
    assert!(ledger.used_bytes() > 0);
    drop(delay);
    assert_eq!(ledger.used_bytes(), 0);
    assert!(
        gain.get_param_mut("invalid").is_none()
            && source.get_param_mut("invalid").is_none()
            && oscillator.get_param_mut("invalid").is_none()
    );
}
#[test]
fn gain_matches_frozen_complete_pcm_and_partial_frames() {
    for rate in [44100, 48000] {
        for ch in [1, 2, 32] {
            for kind in 0..3 {
                let mut node = GainNode::new(10);
                let mut old = gain_before::GainNode::new(10);
                node.set_gain(0.5);
                old.set_gain(0.5);
                assert_eq!(old.gain_value(), 0.5);
                let time = 0.123456789;
                schedule(node.get_param_mut("gain").unwrap(), time, rate, kind);
                schedule(old.get_param_mut("gain").unwrap(), time, rate, kind);
                for frames in [1, 127, 128, 129, 257, 4097] {
                    let input: Vec<_> = (0..frames * ch + ch - 1)
                        .map(|n| (n % 31) as f32 / 31.0 - 0.5)
                        .collect();
                    let mut actual = vec![0.375; input.len()];
                    let mut expected = actual.clone();
                    assert_eq!(
                        cold_process(&mut node, &input, &mut actual, rate, ch, time),
                        old.process(&input, &mut expected, rate, ch, time)
                    );
                    same(&actual, &expected, "chunked gain PCM changed");
                }
            }
        }
    }
}
#[test]
fn delay_matches_frozen_fractional_history_and_empty_tail() {
    for rate in [44100, 48000] {
        for ch in [1, 2, 32] {
            for start in [0.0, 0.123456789] {
                let mut node = DelayNode::new(10, 0.02, rate, ch);
                let mut old = delay_before::DelayNode::new(10, 0.02, rate, ch);
                for n in [&mut node as &mut dyn AudioNodeProcessor, &mut old] {
                    let p = n.get_param_mut("delayTime").unwrap();
                    p.set_value_at_time(0.5 / rate as f32, start);
                    p.linear_ramp_to_value_at_time(0.001, start + 8192.0 / rate as f64);
                }
                let mut elapsed = 0;
                for frames in [1, 127, 128, 129, 257, 4097, 129] {
                    let input: Vec<_> = (0..frames * ch + ch - 1)
                        .map(|n| (n % 37) as f32 / 37.0 - 0.5)
                        .collect();
                    let mut actual = vec![0.375; input.len()];
                    let mut expected = actual.clone();
                    let time = start + elapsed as f64 / rate as f64;
                    assert_eq!(
                        cold_process(&mut node, &input, &mut actual, rate, ch, time),
                        old.process(&input, &mut expected, rate, ch, time)
                    );
                    same(&actual, &expected, "chunked delay PCM or history changed");
                    assert_eq!(node.has_tail_audio(), old.has_tail_audio());
                    elapsed += frames;
                }
                let mut actual = vec![0.375; 8193 * ch as usize];
                let mut expected = actual.clone();
                let time = start + elapsed as f64 / rate as f64;
                assert_eq!(
                    cold_process(&mut node, &[], &mut actual, rate, ch, time),
                    old.process(&[], &mut expected, rate, ch, time)
                );
                same(&actual, &expected, "chunked delay tail changed");
                assert_eq!(node.has_tail_audio(), old.has_tail_audio());
            }
        }
    }
}
#[test]
fn constant_matches_frozen_active_window_and_all_curves() {
    for rate in [44100, 48000] {
        for ch in [1, 2, 32] {
            for kind in 0..3 {
                for start in [0.0, 0.123456789] {
                    let mut node = ConstantSourceNode::new(10);
                    let mut old = constant_before::ConstantSourceNode::new(10);
                    let when = start + 17.0 / rate as f64;
                    node.start(when);
                    old.start(when);
                    node.stop(start + 7000.0 / rate as f64);
                    old.stop(start + 7000.0 / rate as f64);
                    schedule(node.get_param_mut("offset").unwrap(), start, rate, kind);
                    schedule(old.get_param_mut("offset").unwrap(), start, rate, kind);
                    let mut elapsed = 0;
                    for frames in [1, 127, 128, 129, 257, 4097, 4097, 129] {
                        let mut actual = vec![0.375; (frames * ch + ch - 1) as usize];
                        let mut expected = actual.clone();
                        let time = start + elapsed as f64 / rate as f64;
                        assert_eq!(
                            cold_process(&mut node, &[], &mut actual, rate, ch, time),
                            old.process(&[], &mut expected, rate, ch, time)
                        );
                        same(
                            &actual,
                            &expected,
                            "chunked offset PCM or active window changed",
                        );
                        assert_eq!(node.is_finished(), old.is_finished());
                        assert_eq!(node.is_producing(), old.is_producing());
                        elapsed += frames;
                    }
                }
            }
        }
    }
}
#[test]
fn oscillator_matches_frozen_phase_for_all_shapes_and_automation_modes() {
    for rate in [44100, 48000] {
        for ch in [1, 2, 32] {
            for kind in ["sine", "square", "sawtooth", "triangle"] {
                for mode in 0..4 {
                    let start = 0.123456789;
                    let mut node = OscillatorNode::new(10, rate);
                    let mut old = oscillator_before::OscillatorNode::new(10, rate);
                    node.set_type(OscillatorType::from_str(kind));
                    old.set_type(oscillator_before::OscillatorType::from_str(kind));
                    node.start(start + 17.0 / rate as f64);
                    old.start(start + 17.0 / rate as f64);
                    node.stop(start + 7000.0 / rate as f64);
                    old.stop(start + 7000.0 / rate as f64);
                    for n in [&mut node as &mut dyn AudioNodeProcessor, &mut old] {
                        let p = n.get_param_mut("frequency").unwrap();
                        p.set_value(1333.0);
                        if mode & 1 != 0 {
                            p.set_value_at_time(1333.0, start);
                            p.linear_ramp_to_value_at_time(1777.0, start + 8192.0 / rate as f64);
                        }
                        let p = n.get_param_mut("detune").unwrap();
                        p.set_value(300.0);
                        if mode & 2 != 0 {
                            p.set_value_at_time(300.0, start);
                            p.set_target_at_time(-200.0, start + 127.0 / rate as f64, 0.001);
                        }
                    }
                    let mut elapsed = 0;
                    for frames in [1, 127, 128, 129, 257, 4097, 4097, 129] {
                        let mut actual = vec![0.375; (frames * ch + ch - 1) as usize];
                        let mut expected = actual.clone();
                        let time = start + elapsed as f64 / rate as f64;
                        assert_eq!(
                            cold_process(&mut node, &[], &mut actual, rate, ch, time),
                            old.process(&[], &mut expected, rate, ch, time)
                        );
                        same(
                            &actual,
                            &expected,
                            "chunked oscillator PCM or phase changed",
                        );
                        assert_eq!(node.is_finished(), old.is_finished());
                        assert_eq!(node.is_producing(), old.is_producing());
                        elapsed += frames;
                    }
                }
            }
        }
    }
}
#[test]
fn offset_timeline_matches_independent_before_without_rebasing_clock() {
    for rate in [0, 1, 44100, 48000, u32::MAX] {
        for start in [0.0, 17.0 / 44100.0, 0.123456789, 123456.123456789] {
            for kind in 0..5 {
                let mut current = AudioParamTimeline::new(0.5, -10.0, 10.0);
                let mut old = param_before::AudioParamTimeline::new(0.5, -10.0, 10.0);
                let sr = rate.max(1) as f64;
                macro_rules! both {($method:ident($($arg:expr),*))=>{{current.$method($($arg),*);old.$method($($arg),*);}};}
                both!(set_value(0.25));
                assert_eq!(current.default_value(), old.default_value());
                assert_eq!(current.min_value(), old.min_value());
                assert_eq!(current.max_value(), old.max_value());
                assert_eq!(current.value(), old.value());
                assert_eq!(current.has_automation(), old.has_automation());
                if kind > 0 {
                    both!(set_value_at_time(0.25, start));
                    both!(set_value_at_time(0.5, start));
                }
                match kind {
                    1 => both!(linear_ramp_to_value_at_time(0.875, start + 8192.0 / sr)),
                    2 => both!(exponential_ramp_to_value_at_time(
                        0.875,
                        start + 8192.0 / sr
                    )),
                    3 => both!(set_target_at_time(0.875, start + 127.0 / sr, 0.001)),
                    4 => {
                        both!(linear_ramp_to_value_at_time(0.875, start + 8192.0 / sr));
                        both!(set_value_now(0.375, start + 129.0 / sr));
                        both!(cancel_scheduled_values(start + 8193.0 / sr));
                        both!(gc_events(start + 258.0 / sr));
                    }
                    _ => {}
                }
                assert_eq!(
                    current.compute_value(start).to_bits(),
                    old.compute_value(start).to_bits()
                );
                for frames in [1, 127, 128, 129, 257, 4097] {
                    let mut expected = vec![0.0; frames];
                    old.compute_values(start, &mut expected, rate);
                    let mut actual = vec![0.0; frames];
                    current.compute_values(start, &mut actual, rate);
                    same(&actual, &expected, "full timeline extraction changed PCM");
                    actual.fill(0.0);
                    for offset in (0..frames).step_by(128) {
                        let end = (offset + 128).min(frames);
                        current.compute_values_offset(
                            start,
                            offset,
                            &mut actual[offset..end],
                            rate,
                        );
                    }
                    same(
                        &actual,
                        &expected,
                        "chunked timeline changed original frame clock",
                    );
                    actual.fill(0.0);
                    let mut block = current.values_block(start, rate);
                    for offset in (0..frames).step_by(128) {
                        let end = (offset + 128).min(frames);
                        current.compute_values_from(&mut block, offset, &mut actual[offset..end]);
                    }
                    same(&actual, &expected, "persistent timeline cursor changed PCM");
                }
            }
        }
    }
}
