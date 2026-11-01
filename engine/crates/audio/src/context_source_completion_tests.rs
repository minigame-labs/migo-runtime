//! Natural EOF and duration must finish in the block that consumed their last sample.
use super::*;
use migo_alloc_probe::thread_counts;

fn render(
    node: &mut BufferSourceNode,
    output: &mut [f32],
    rate: u32,
    channels: u32,
    time: f64,
) -> usize {
    let before = thread_counts();
    let written = node.process(&[], output, rate, channels, time);
    let after = thread_counts();
    assert_eq!(
        (after.allocations, after.reallocations),
        (before.allocations, before.reallocations),
        "source completion render allocated"
    );
    written
}

pub(super) fn natural_and_duration() {
    let probe = thread_counts();
    std::hint::black_box(vec![0_u8; 64]);
    assert!(thread_counts().allocations > probe.allocations);
    // buffer frames, buffer/context rate ratio, speed, detune, offset frames,
    // start context frame, duration buffer frames, loop.
    let cases = [
        (128, 1.0, 1.0, 0.0, 0.0, 0.0, None, false),
        (256, 1.0, 1.0, 0.0, 0.0, 0.0, None, false),
        (128, 1.0, 0.5, 0.0, 0.0, 0.0, None, false),
        (256, 1.0, 2.0, 0.0, 0.0, 0.0, None, false),
        (256, 2.0, 1.0, 0.0, 0.0, 0.0, None, false),
        (64, 0.5, 1.0, 0.0, 0.0, 0.0, None, false),
        (256, 1.0, 1.0, 1200.0, 0.0, 0.0, None, false),
        (257, 1.0, 1.0, 0.0, 1.0, 0.0, None, false),
        (128, 1.0, 1.0, 0.0, 0.0, 17.5, None, false),
        (7, 1.0, 0.5, 0.0, 1.25, 0.0, None, false),
        (512, 1.0, 1.0, 0.0, 0.0, 0.0, Some(128.0), false),
        (512, 1.0, 1.0, 0.0, 0.0, 0.0, Some(128.0), true),
        (4, 1.0, 1.0, 0.0, 0.0, 0.0, Some(256.0), true),
        (4, 1.0, 2.0, 0.0, 0.0, 0.0, Some(256.0), true),
        (4, 1.0, 0.5, 0.0, 0.0, 0.0, Some(128.0), true),
        (4, 1.0, 1.0, 0.0, 0.0, 0.0, Some(128.25), true),
        (4, 1.0, 0.0, 0.0, 0.0, 0.0, Some(128.0), true),
        (128, 1.0, 0.0, 0.0, 0.0, 0.0, Some(0.0), false),
    ];
    for rate in [44_100, 48_000] {
        for channels in [1, 2, 32] {
            for (frames, ratio, speed, detune, offset, start, duration, looping) in cases {
                let buffer_rate = (rate as f64 * ratio) as u32;
                let audio = DecodedAudio {
                    samples: (0..frames).map(|n| 0.25 + n as f32 / 1024.0).collect(),
                    sample_rate: buffer_rate,
                    channels: 1,
                };
                let retained = crate::limits::RetainedAudio::try_new(
                    audio,
                    &crate::limits::PcmBudget::for_context(),
                )
                .unwrap();
                let mut node = BufferSourceNode::new(10, rate);
                node.set_buffer(Some(Arc::new(retained)));
                node.set_playback_rate(speed);
                node.get_param_mut("detune").unwrap().set_value(detune);
                node.set_loop(looping, 0.0, 0.0);
                let when = start / rate as f64;
                let duration = duration.map(|n| n / buffer_rate as f64);
                node.start(when, offset / buffer_rate as f64, duration, 0.0);
                let step = speed as f64 * 2.0_f64.powf(detune as f64 / 1200.0) * ratio;
                let first = start.ceil() as usize;
                let delay = (first as f64 / rate as f64 - when).max(0.0) * step * rate as f64;
                let initial = (offset / buffer_rate as f64) * buffer_rate as f64 + delay;
                let limit = duration.map(|d| d * buffer_rate as f64);
                let remaining = limit.unwrap_or(f64::INFINITY).min(if looping {
                    f64::INFINITY
                } else {
                    frames as f64 - (offset / buffer_rate as f64) * buffer_rate as f64
                });
                let active = if remaining <= delay {
                    0
                } else if step == 0.0 {
                    usize::MAX
                } else {
                    ((remaining - delay) / step).ceil() as usize
                };
                let mut output = vec![0.375; 128 * channels as usize + channels as usize - 1];
                for block in 0_usize..4 {
                    output.fill(0.375);
                    let actual = render(
                        &mut node,
                        &mut output,
                        rate,
                        channels,
                        (block * 128) as f64 / rate as f64,
                    );
                    let mut expected_count = 0;
                    for frame in 0..128 {
                        let global = block * 128 + frame;
                        let generated = global.saturating_sub(first);
                        let sample = if global >= first && generated < active {
                            expected_count += 1;
                            let position = initial + generated as f64 * step;
                            let position = if looping {
                                position.rem_euclid(frames as f64)
                            } else {
                                position
                            };
                            let base = position.floor() as usize;
                            let successor = if looping {
                                (base + 1) % frames
                            } else {
                                (base + 1).min(frames - 1)
                            };
                            let a = 0.25 + base as f32 / 1024.0;
                            let b = 0.25 + successor as f32 / 1024.0;
                            a + (b - a) * (position - base as f64) as f32
                        } else {
                            0.0
                        };
                        assert!(
                            output[frame * channels as usize..(frame + 1) * channels as usize]
                                .iter()
                                .all(|v| v.to_bits() == sample.to_bits()),
                            "natural completion PCM changed"
                        );
                    }
                    assert_eq!(
                        actual, expected_count,
                        "natural completion frame count changed"
                    );
                    assert!(
                        output[128 * channels as usize..]
                            .iter()
                            .all(|v| *v == 0.375)
                    );
                    let finished = active != usize::MAX
                        && (block + 1) * 128 >= first + active
                        && (block + 1) * 128 > first;
                    assert_eq!(
                        node.is_finished(),
                        finished,
                        "source natural completion missed its final block: frames {frames} ratio {ratio} speed {speed} detune {detune} duration {duration:?} loop {looping} block {block}"
                    );
                    assert_eq!(node.is_producing(), !finished);
                }
            }
        }
    }
}

pub(super) fn unbound_and_collection() {
    for rate in [44_100, 48_000] {
        for start in [0.0_f64, 128.0, 257.5] {
            let mut node = BufferSourceNode::new(10, rate);
            node.start(start / rate as f64, 0.0, None, 0.0);
            for block in 0_usize..4 {
                let mut out = [0.375; 128];
                assert_eq!(
                    render(
                        &mut node,
                        &mut out,
                        rate,
                        1,
                        (block * 128) as f64 / rate as f64
                    ),
                    0
                );
                assert_eq!(out, [0.0; 128]);
                assert_eq!(
                    node.is_finished(),
                    (block + 1) as f64 * 128.0 > start.ceil(),
                    "unbound source did not finish on its first active block"
                );
            }
            node.start(1000.0, 0.0, None, 0.0);
            node.stop(1000.0);
            assert!(node.is_finished(), "finished unbound source restarted");
        }
        // Binding after start remains valid until the first active render.
        for after_silent_block in [false, true] {
            let mut node = BufferSourceNode::new(10, rate);
            node.start(128.0 / rate as f64, 0.0, None, 0.0);
            if after_silent_block {
                let mut silent = [0.375; 128];
                assert_eq!(render(&mut node, &mut silent, rate, 1, 0.0), 0);
                assert_eq!(silent, [0.0; 128]);
                assert!(
                    !node.is_finished(),
                    "future unbound source finished before start"
                );
            }
            let audio = DecodedAudio {
                samples: vec![0.25; 128],
                sample_rate: rate,
                channels: 1,
            };
            let retained = crate::limits::RetainedAudio::try_new(
                audio,
                &crate::limits::PcmBudget::for_context(),
            )
            .unwrap();
            node.set_buffer(Some(Arc::new(retained)));
            let mut out = [0.375; 128];
            assert_eq!(
                render(&mut node, &mut out, rate, 1, 128.0 / rate as f64),
                128
            );
            assert_eq!(
                out, [0.25; 128],
                "late-bound source lost its final PCM block"
            );
            assert!(
                node.is_finished(),
                "late-bound source missed its natural completion"
            );
        }
        for channels in [1, 2, 32] {
            for frames in [1, 127, 128, 129, 256] {
                for start in [0.0_f64, 17.5] {
                    let mut ctx = AudioContext::new(1, rate, channels);
                    let buffer = ctx
                        .add_buffer(DecodedAudio {
                            samples: (0..frames).map(|n| 0.25 + n as f32 / 1024.0).collect(),
                            sample_rate: rate,
                            channels: 1,
                        })
                        .unwrap();
                    ctx.create_buffer_source(10);
                    ctx.set_buffer(10, Some(buffer));
                    ctx.create_gain(11);
                    ctx.set_gain(11, 0.5);
                    ctx.connect(10, 11);
                    ctx.connect(11, DESTINATION_NODE_ID);
                    ctx.watch_source_ended(10, true);
                    assert!(ctx.start_source(10, start / rate as f64, 0.0, None));
                    ctx.release_node(11);
                    let first = start.ceil() as usize;
                    let first_position =
                        (first as f64 / rate as f64 - start / rate as f64) * rate as f64;
                    let end = first + (frames as f64 - first_position).ceil() as usize;
                    let mut out = vec![0.125; 128 * channels as usize];
                    let mut all_ended = Vec::new();
                    let mut all_collected = Vec::new();
                    for block in 0_usize..4 {
                        out.fill(0.125);
                        let before = thread_counts();
                        let ids = ctx.process_native_device(&mut out, rate, channels).unwrap();
                        let after = thread_counts();
                        assert_eq!(
                            (after.allocations, after.reallocations),
                            (before.allocations, before.reallocations),
                            "natural source collection allocated"
                        );
                        all_collected.extend_from_slice(ids);
                        all_ended.extend(ctx.drain_ended());
                        for frame in 0..128 {
                            let global = block * 128 + frame;
                            let expected = if global >= first && global < end {
                                let position = first_position + (global - first) as f64;
                                let sample =
                                    0.25 + position.min((frames - 1) as f64) as f32 / 1024.0;
                                0.125 + 0.5 * sample
                            } else {
                                0.125
                            };
                            assert!(
                                out[frame * channels as usize..(frame + 1) * channels as usize]
                                    .iter()
                                    .all(|v| v.to_bits() == expected.to_bits()),
                                "natural source final graph PCM changed"
                            );
                        }
                        let finished = (block + 1) * 128 >= end;
                        assert_eq!(
                            all_ended,
                            if finished { vec![10] } else { vec![] },
                            "natural source ended was not delivered in its final block"
                        );
                        assert_eq!(
                            all_collected.len(),
                            if finished { 2 } else { 0 },
                            "natural source chain was not collected in its final block"
                        );
                        assert_eq!(ctx.has_active_sources(), !finished);
                    }
                    all_collected.sort_unstable();
                    assert_eq!(all_collected, vec![10, 11]);
                }
            }
        }
    }
}
