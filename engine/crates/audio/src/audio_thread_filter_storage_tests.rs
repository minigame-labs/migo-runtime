use super::*;
use crate::nodes::AudioNodeProcessor;
#[path = "nodes/filter_storage_biquad_filter_reference.rs"]
mod biquad_before;
#[path = "nodes/filter_storage_iir_filter_reference.rs"]
mod iir_before;

#[test]
fn biquad_factory_binds_parameter_range_to_context_rate() {
    let mut ctx = graph_with_rate(2, 44100);
    assert!(ctx.create_biquad_filter(20));
    let mut expected = biquad_before::BiquadFilterNode::new(20, 2, 44100);
    expected
        .get_param_mut("frequency")
        .unwrap()
        .set_value(23000.0);
    ctx.with_node_typed::<BiquadFilterNode, _>(20, |n| {
        n.get_param_mut("frequency").unwrap().set_value(23000.0);
        let a = n.get_frequency_response(44100.0, &[1000.0, 22050.0, 23000.0]);
        let b = expected.get_frequency_response(44100.0, &[1000.0, 22050.0, 23000.0]);
        assert!(
            a.0.iter()
                .zip(&b.0)
                .chain(a.1.iter().zip(&b.1))
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "biquad factory used device rate instead of context rate"
        );
    });
    ctx.close();
}

#[test]
fn biquad_rate_alone_invalidates_cached_coefficients() {
    let mut current = BiquadFilterNode::new(20, 2, 48000);
    let mut expected = biquad_before::BiquadFilterNode::new(20, 2, 48000);
    let input: Vec<_> = (0..256).map(|i| (i % 37) as f32 / 37.0 - 0.5).collect();
    let mut actual = vec![0.0; input.len()];
    let mut reference = actual.clone();
    for rate in [44100, 32000, 48000, 44100] {
        // Force only the old reference to calculate for the current rate,
        // preserving its histories and every parameter value.
        expected.set_type(biquad_before::BiquadFilterType::Lowpass);
        let a = current.get_frequency_response(rate as f64, &[350.0, 1000.0, 12000.0]);
        let b = expected.get_frequency_response(rate as f64, &[350.0, 1000.0, 12000.0]);
        assert!(
            a.0.iter()
                .zip(&b.0)
                .chain(a.1.iter().zip(&b.1))
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "biquad reused coefficients from another sample rate"
        );
        current.process(&input, &mut actual, rate, 2, 0.0);
        expected.process(&input, &mut reference, rate, 2, 0.0);
        same(&actual, &reference);
    }
}
fn graph(channels: u32) -> AudioContext {
    graph_with_rate(channels, 48000)
}
fn graph_with_rate(channels: u32, rate: u32) -> AudioContext {
    AudioContext::new(1, rate, channels)
}
fn same(a: &[f32], b: &[f32]) {
    assert!(
        a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.to_bits() == b.to_bits()),
        "filter retained storage changed complete PCM or response"
    );
}
#[test]
fn iir_pcm_and_response_match_frozen_before_for_all_history_shapes() {
    for channels in [1, 2, 32] {
        for (ff, fb) in [
            (vec![0.25, 0.5, 0.25], vec![1.0, -0.4, 0.1]),
            (vec![0.6, 0.25, -0.1], vec![2.0, -0.3, 0.15]),
            (vec![0.4, -0.1], vec![-2.0, 0.3]),
            (vec![], vec![1.0]),
            (vec![1.0], vec![]),
            (vec![], vec![]),
            (vec![0.05; 20], vec![1.0]),
        ] {
            let mut ctx = graph(channels);
            assert!(ctx.create_iir_filter(20, &ff, &fb));
            let mut reference =
                iir_before::IIRFilterNode::new(20, ff.clone(), fb.clone(), channels);
            let input: Vec<_> = (0..128 * channels)
                .map(|i| (i % 37) as f32 / 37.0 - 0.5)
                .collect();
            let mut actual = vec![0.0; input.len()];
            let mut expected = actual.clone();
            ctx.with_node_typed::<IIRFilterNode, _>(20, |n| {
                for block in 0..5 {
                    let data = if block < 3 { input.as_slice() } else { &[] };
                    let before = migo_alloc_probe::thread_counts();
                    n.process(data, &mut actual, 48000, channels, 0.0);
                    let after = migo_alloc_probe::thread_counts();
                    assert_eq!(
                        (
                            after.allocations - before.allocations,
                            after.reallocations - before.reallocations
                        ),
                        (0, 0),
                        "IIR first/steady render allocated"
                    );
                    reference.process(data, &mut expected, 48000, channels, 0.0);
                    same(&actual, &expected);
                    assert_eq!(n.has_tail_audio(), reference.has_tail_audio());
                }
                let freq = [-0.0, 0.0, 1000.0, 12000.0, 24000.0];
                let a = n.get_frequency_response(48000.0, &freq);
                let b = reference.get_frequency_response(48000.0, &freq);
                same(&a.0, &b.0);
                same(&a.1, &b.1);
            });
            ctx.close();
        }
    }
}
#[test]
fn biquad_pcm_and_response_match_frozen_before_for_every_filter_type() {
    for rate in [44100, 48000] {
        for channels in [1, 2, 32] {
            for kind in [
                "lowpass",
                "highpass",
                "bandpass",
                "lowshelf",
                "highshelf",
                "peaking",
                "notch",
                "allpass",
            ] {
                let mut ctx = graph_with_rate(channels, rate);
                assert!(ctx.create_biquad_filter(20));
                let mut reference = biquad_before::BiquadFilterNode::new(20, channels, rate);
                reference.set_type(biquad_before::BiquadFilterType::from_str(kind));
                for (key, value) in [
                    ("frequency", 1333.0),
                    ("detune", 300.0),
                    ("Q", 0.707),
                    ("gain", 6.0),
                ] {
                    reference.get_param_mut(key).unwrap().set_value(value);
                }
                let input: Vec<_> = (0..128 * channels)
                    .map(|i| (i % 37) as f32 / 37.0 - 0.5)
                    .collect();
                let mut actual = vec![0.0; input.len()];
                let mut expected = actual.clone();
                ctx.with_node_typed::<BiquadFilterNode, _>(20, |n| {
                    n.set_type(crate::nodes::BiquadFilterType::from_str(kind));
                    for (key, value) in [
                        ("frequency", 1333.0),
                        ("detune", 300.0),
                        ("Q", 0.707),
                        ("gain", 6.0),
                    ] {
                        n.get_param_mut(key).unwrap().set_value(value);
                    }
                    for block in 0..5 {
                        let data = if block < 3 { input.as_slice() } else { &[] };
                        let before = migo_alloc_probe::thread_counts();
                        n.process(data, &mut actual, rate, channels, 0.0);
                        let after = migo_alloc_probe::thread_counts();
                        assert_eq!(
                            (
                                after.allocations - before.allocations,
                                after.reallocations - before.reallocations
                            ),
                            (0, 0),
                            "biquad first/steady render allocated"
                        );
                        reference.process(data, &mut expected, rate, channels, 0.0);
                        same(&actual, &expected);
                        assert_eq!(n.has_tail_audio(), reference.has_tail_audio());
                    }
                    let freq = [-0.0, 0.0, 1000.0, 12000.0, 24000.0];
                    let a = n.get_frequency_response(rate as f64, &freq);
                    let b = reference.get_frequency_response(rate as f64, &freq);
                    same(&a.0, &b.0);
                    same(&a.1, &b.1);
                });
                ctx.close();
            }
        }
    }
}
