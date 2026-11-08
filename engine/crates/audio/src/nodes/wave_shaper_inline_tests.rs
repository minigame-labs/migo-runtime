//! A frozen full-block kernel guards chunk order, history and exact FIR bits.
use super::*;
use migo_alloc_probe::thread_counts;
#[path = "wave_shaper_inline_reference.rs"]
pub(crate) mod before;

fn same(actual: &[f32], expected: &[f32], message: &str) {
    assert!(
        actual.len() == expected.len()
            && actual
                .iter()
                .zip(expected)
                .all(|(a, b)| a.to_bits() == b.to_bits()),
        "{message}"
    );
}
fn curves() -> Vec<Option<Vec<f32>>> {
    vec![
        None,
        Some(vec![]),
        Some(vec![0.25]),
        Some(vec![-1.0, 1.0]),
        Some(vec![-0.5, 0.25, 1.0]),
        Some(
            (0..1024)
                .map(|i| ((i as f32 / 1023.0 * 2.0 - 1.0) * 4.0).clamp(-1.0, 1.0))
                .collect(),
        ),
    ]
}
fn render(
    node: &mut WaveShaperNode,
    old: &mut before::WaveShaperNode,
    input: &[f32],
    output: &mut [f32],
    rate: u32,
    channels: u32,
) {
    let mut expected = output.to_vec();
    let probe = thread_counts();
    let frames = node.process(input, output, rate, channels, 0.0);
    let after = thread_counts();
    assert_eq!(
        (
            after.allocations - probe.allocations,
            after.reallocations - probe.reallocations
        ),
        (0, 0),
        "long waveshaper render allocated scratch"
    );
    assert_eq!(
        frames,
        old.process(input, &mut expected, rate, channels, 0.0),
        "bounded waveshaper frame count changed"
    );
    same(
        output,
        &expected,
        "bounded waveshaper PCM or history changed",
    );
    assert_eq!(
        node.has_tail_audio(),
        old.has_tail_audio(),
        "bounded waveshaper tail changed"
    );
    assert_eq!(
        node.is_producing(),
        old.is_producing(),
        "bounded waveshaper activity changed"
    );
}
#[test]
fn coefficients_and_mode_setter_are_exact_without_heap() {
    for factor in [2, 4] {
        for gain in [0.0, 1.0, 2.0, 4.0, -1.0] {
            let probe = thread_counts();
            let actual = design_lowpass(0.5 / factor as f64, gain);
            let after = thread_counts();
            assert_eq!(
                (
                    after.allocations - probe.allocations,
                    after.reallocations - probe.reallocations
                ),
                (0, 0),
                "FIR coefficient design allocated"
            );
            same(
                &actual,
                &before::coefficients(0.5 / factor as f64, gain),
                "FIR coefficient bits changed",
            );
        }
    }
    let mut node = WaveShaperNode::new(1);
    // The first oversampled mode sizes the per-channel FIR history once on the
    // command path; every later mode change only redesigns the fixed taps.
    node.set_oversample(OversampleType::TwoX, 1);
    for mode in ["2x", "2x", "4x", "none", "none", "4x", "2x"] {
        let probe = thread_counts();
        node.set_oversample(OversampleType::from_str(mode), 1);
        let after = thread_counts();
        assert_eq!(
            (
                after.allocations - probe.allocations,
                after.reallocations - probe.reallocations,
                after.bytes_freed - probe.bytes_freed
            ),
            (0, 0, 0),
            "waveshaper mode setter allocated taps"
        );
        if mode != "none" {
            let factor = if mode == "2x" { 2.0 } else { 4.0 };
            same(
                &node.up_taps,
                &before::coefficients(0.5 / factor, factor),
                "FIR coefficient bits changed",
            );
            same(
                &node.down_taps,
                &before::coefficients(0.5 / factor, 1.0),
                "FIR coefficient bits changed",
            );
        }
    }
}
#[test]
fn modes_and_partial_frames_match_frozen_full_block_kernel() {
    for rate in [44100, 48000] {
        for channels in [1, 2, 32] {
            for curve in curves() {
                let mut node = WaveShaperNode::new(1);
                let mut old = before::WaveShaperNode::new(1);
                node.set_curve(curve.clone());
                old.set_curve(curve);
                node.set_output_connected(true);
                old.set_output_connected(true);
                for (mode, frames) in [
                    ("2x", 1),
                    ("2x", 17),
                    ("4x", 127),
                    ("4x", 128),
                    ("none", 129),
                    ("2x", 257),
                    ("2x", 129),
                    ("4x", 257),
                ] {
                    node.set_oversample(OversampleType::from_str(mode), 1);
                    old.set_oversample(before::OversampleType::from_str(mode));
                    node.prepare_oversampling(channels);
                    old.prepare_oversampling(channels);
                    let input: Vec<_> = (0..frames * channels + channels - 1)
                        .map(|i| (i % 37) as f32 / 37.0 * 3.0 - 1.5)
                        .collect();
                    let mut output = vec![0.375; input.len()];
                    render(&mut node, &mut old, &input, &mut output, rate, channels);
                }
                let mut output = vec![0.375; 257 * channels as usize + channels as usize - 1];
                render(&mut node, &mut old, &[], &mut output, rate, channels);
            }
        }
    }
}
#[test]
fn long_blocks_and_changing_channels_keep_history_without_scratch_heap() {
    for rate in [44100, 48000] {
        for mode in ["2x", "4x"] {
            for curve in [
                Some(vec![]),
                Some(vec![0.25]),
                Some(vec![-0.5, 0.25, 1.0]),
                Some(vec![-1.0, -1.0, 0.0, 1.0, 1.0]),
            ] {
                let mut node = WaveShaperNode::new(1);
                let mut old = before::WaveShaperNode::new(1);
                node.set_curve(curve.clone());
                old.set_curve(curve);
                node.set_oversample(OversampleType::from_str(mode), 1);
                old.set_oversample(before::OversampleType::from_str(mode));
                node.prepare_oversampling(32);
                old.prepare_oversampling(32);
                for (channels, frames) in [
                    (1, 4097),
                    (2, 257),
                    (32, 129),
                    (2, 4097),
                    (1, 257),
                    (32, 257),
                ] {
                    let input: Vec<_> = (0..frames * channels + channels - 1)
                        .map(|i| (i % 31) as f32 / 31.0 * 3.0 - 1.5)
                        .collect();
                    let mut output = vec![0.375; input.len()];
                    render(&mut node, &mut old, &input, &mut output, rate, channels);
                }
            }
        }
    }
}
#[test]
fn curve_replacements_dc_disconnect_and_empty_tail_match_before() {
    for channels in [1, 2, 32] {
        for mode in ["none", "2x", "4x"] {
            let mut node = WaveShaperNode::new(1);
            let mut old = before::WaveShaperNode::new(1);
            node.set_oversample(OversampleType::from_str(mode), 1);
            old.set_oversample(before::OversampleType::from_str(mode));
            node.prepare_oversampling(channels);
            old.prepare_oversampling(channels);
            for (index, curve) in curves()
                .into_iter()
                .chain([None, Some(vec![0.25])])
                .enumerate()
            {
                node.set_curve(curve.clone());
                old.set_curve(curve);
                let connected = index % 2 == 0;
                node.set_output_connected(connected);
                old.set_output_connected(connected);
                let input: Vec<_> = (0..129 * channels)
                    .map(|i| (i % 17) as f32 / 17.0 - 0.5)
                    .collect();
                let mut output = vec![0.375; input.len()];
                render(&mut node, &mut old, &input, &mut output, 48000, channels);
                for _ in 0..3 {
                    output.fill(0.375);
                    render(&mut node, &mut old, &[], &mut output, 48000, channels);
                }
            }
        }
    }
}
