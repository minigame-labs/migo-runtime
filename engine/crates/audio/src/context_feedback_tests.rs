//! Rendering-loop cycle rules: mute residual cycles and split cyclic Delay I/O.
use super::*;
use crate::nodes::{ConstantSourceNode, DelayNode};

fn constant(ctx: &mut AudioContext, id: u32, value: f32) {
    let mut source = ConstantSourceNode::new(id);
    source.get_param_mut("offset").unwrap().set_value(value);
    source.start(0.0);
    assert!(ctx.add_node(Box::new(source)));
}
fn impulse(ctx: &mut AudioContext, id: u32) {
    let buffer = ctx
        .create_empty_buffer(ctx.channels, 1, ctx.sample_rate)
        .unwrap();
    for ch in 0..ctx.channels {
        ctx.copy_to_channel(buffer, &[1.0], ch, 0).unwrap();
    }
    assert!(ctx.create_buffer_source(id));
    assert!(ctx.set_buffer(id, Some(buffer)));
    assert!(ctx.start_source(id, 0.0, 0.0, None));
}
fn feedback(ctx: &mut AudioContext, delays: &[u32], gain: u32) {
    for &id in delays {
        assert!(ctx.add_node(Box::new(DelayNode::new(
            id,
            0.001,
            ctx.sample_rate,
            ctx.channels
        ))));
    }
    assert!(ctx.create_gain(gain));
    ctx.nodes
        .get_mut(&gain)
        .unwrap()
        .get_param_mut("gain")
        .unwrap()
        .set_value(0.5);
    for pair in delays.windows(2) {
        ctx.connect(pair[0], pair[1]);
    }
    ctx.connect(*delays.last().unwrap(), gain);
    ctx.connect(gain, delays[0]);
    ctx.connect(*delays.last().unwrap(), DESTINATION_NODE_ID);
}
#[test]
fn delay_free_cycle_is_muted_and_its_downstream_keeps_external_audio() {
    let mut ctx = AudioContext::new(1, 48000, 1);
    ctx.set_test_dense_order();
    constant(&mut ctx, 50, 1.0);
    constant(&mut ctx, 51, 0.25);
    for id in [10, 11, 12] {
        assert!(ctx.create_gain(id));
    }
    ctx.connect(50, 10);
    ctx.connect(10, 11);
    ctx.connect(11, 12);
    ctx.connect(51, 12);
    ctx.connect(12, DESTINATION_NODE_ID);
    let mut prior = [0.0; 128];
    ctx.process(&mut prior);
    assert_eq!(prior, [1.25; 128]);
    ctx.connect(11, 10);
    for _ in 0..4 {
        let mut out = [0.0; 128];
        ctx.process(&mut out);
        assert!(
            out.iter().all(|x| x.to_bits() == 0.25f32.to_bits()),
            "delay-free cycle leaked audio or muted its downstream"
        );
    }
}
#[test]
fn cyclic_zero_delay_starts_with_exactly_one_silent_quantum() {
    let mut ctx = AudioContext::new(1, 48000, 1);
    ctx.set_test_dense_order();
    impulse(&mut ctx, 50);
    feedback(&mut ctx, &[10], 11);
    ctx.connect(50, 10);
    for block in 0..5 {
        let mut out = [0.0; 128];
        ctx.process(&mut out);
        let mut expected = [0.0; 128];
        if block > 0 {
            expected[0] = 0.5f32.powi(block - 1);
        }
        assert_eq!(
            out.map(f32::to_bits),
            expected.map(f32::to_bits),
            "cyclic Delay did not enforce one quantum before output"
        );
    }
}
#[test]
fn every_delay_in_the_cycle_contributes_its_own_quantum() {
    let mut ctx = AudioContext::new(1, 48000, 1);
    ctx.set_test_dense_order();
    impulse(&mut ctx, 50);
    feedback(&mut ctx, &[10, 11], 12);
    ctx.connect(50, 10);
    for block in 0..7 {
        let mut out = [0.0; 128];
        ctx.process(&mut out);
        let mut expected = [0.0; 128];
        if block >= 2 && block % 2 == 0 {
            expected[0] = 0.5f32.powi(block / 2 - 1);
        }
        assert_eq!(
            out.map(f32::to_bits),
            expected.map(f32::to_bits),
            "multiple cycle breakers changed the exact feedback latency"
        );
    }
}

#[test]
fn a_delay_in_the_component_does_not_legalize_a_delay_free_subcycle() {
    let mut ctx = AudioContext::new(1, 48000, 1);
    constant(&mut ctx, 50, 1.0);
    feedback(&mut ctx, &[10], 11);
    assert!(ctx.create_gain(12));
    ctx.connect(50, 10);
    ctx.connect(11, 12);
    ctx.connect(12, 11);
    for block in 0..4 {
        let mut out = [0.0; 128];
        ctx.process(&mut out);
        let value = if block == 0 { 0.0 } else { 1.0 };
        assert!(
            out.iter().all(|&x| x == value),
            "residual cycle inside a Delay component was not muted"
        );
    }
}

#[test]
fn an_acyclic_delay_downstream_of_a_muted_cycle_keeps_zero_delay() {
    let mut ctx = AudioContext::new(1, 48000, 1);
    constant(&mut ctx, 50, 1.0);
    constant(&mut ctx, 51, 0.25);
    for id in [10, 11] {
        assert!(ctx.create_gain(id));
    }
    assert!(ctx.add_node(Box::new(DelayNode::new(12, 0.001, 48000, 1))));
    ctx.connect(50, 10);
    ctx.connect(10, 11);
    ctx.connect(11, 10);
    ctx.connect(11, 12);
    ctx.connect(51, 12);
    ctx.connect(12, DESTINATION_NODE_ID);
    let mut out = [0.0; 128];
    ctx.process(&mut out);
    assert!(
        out.iter().all(|&x| x == 0.25),
        "acyclic downstream delay was incorrectly split or muted"
    );
}

#[test]
fn a_delay_self_loop_is_a_cycle_breaker() {
    let mut ctx = AudioContext::new(1, 48000, 1);
    impulse(&mut ctx, 50);
    assert!(ctx.add_node(Box::new(DelayNode::new(10, 0.001, 48000, 1))));
    ctx.connect(50, 10);
    ctx.connect(10, 10);
    ctx.connect(10, DESTINATION_NODE_ID);
    for block in 0..4 {
        let mut out = [0.0; 128];
        ctx.process(&mut out);
        let mut expected = [0.0; 128];
        if block > 0 {
            expected[0] = 1.0;
        }
        assert_eq!(out, expected, "Delay self-loop lost the minimum quantum");
    }
}

#[test]
fn connecting_and_disconnecting_a_cycle_reclassifies_the_original_delay() {
    let mut ctx = AudioContext::new(1, 48000, 1);
    constant(&mut ctx, 50, 1.0);
    assert!(ctx.add_node(Box::new(DelayNode::new(10, 0.001, 48000, 1))));
    assert!(ctx.create_gain(11));
    ctx.connect(50, 10);
    ctx.connect(10, 11);
    ctx.connect(10, DESTINATION_NODE_ID);
    let mut out = [0.0; 128];
    ctx.process(&mut out);
    assert_eq!(out, [1.0; 128]);
    ctx.connect(11, 10);
    assert!(ctx.set_node_param(50, "offset", 0.25));
    out.fill(0.0);
    ctx.process(&mut out);
    assert_eq!(
        out, [1.0; 128],
        "cycle entry did not preserve the original delay history"
    );
    ctx.disconnect(11, Some(10));
    assert!(ctx.set_node_param(50, "offset", 0.5));
    out.fill(0.0);
    ctx.process(&mut out);
    assert_eq!(
        out, [0.5; 128],
        "cycle removal left a delay reader or stale mute flag behind"
    );
}

#[test]
fn cold_cycle_rebuild_and_full_budget_closed_cpu_render_do_not_allocate() {
    use crate::limits::AudioAggregateLedger;
    let scheduler = migo_io::scheduler::IoScheduler::new(1);
    let ledger = Arc::new(AudioAggregateLedger::new(64 << 20));
    let mut ctx =
        AudioContext::new_managed(1, 48000, 1, scheduler.audio_cpu(), ledger.clone()).unwrap();
    constant(&mut ctx, 50, 1.0);
    assert!(ctx.add_node(Box::new(DelayNode::new(10, 0.001, 48000, 1))));
    ctx.connect(50, 10);
    ctx.connect(10, 100);
    for id in 100..356 {
        assert!(ctx.create_gain(id));
        if id > 100 {
            ctx.connect(id - 1, id);
        }
    }
    ctx.connect(355, 10);
    ctx.connect(10, DESTINATION_NODE_ID);
    let peer = ledger
        .try_reserve((64 << 20) - ledger.used_bytes(), "feedback full peer")
        .unwrap();
    scheduler.close();
    let mut out = [0.0; 128];
    let before = migo_alloc_probe::thread_counts();
    ctx.process(&mut out);
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        after.allocation_events(),
        before.allocation_events(),
        "cold SCC/Delay graph allocated on render"
    );
    assert_eq!(out, [0.0; 128]);
    ctx.disconnect(355, Some(10));
    out.fill(0.0);
    let before = migo_alloc_probe::thread_counts();
    ctx.process(&mut out);
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        after.allocation_events(),
        before.allocation_events(),
        "closed-CPU SCC rebuild allocated on render"
    );
    assert_eq!(out, [1.0; 128]);
    drop(peer);
    ctx.close();
    drop(ctx);
    let start = std::time::Instant::now();
    while ledger.used_bytes() != 0 {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(15),
            "feedback storage credit survived original CPU destruction"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[test]
fn released_feedback_tail_is_not_truncated_at_the_old_epsilon() {
    let mut ctx = AudioContext::new(1, 48000, 1);
    impulse(&mut ctx, 50);
    feedback(&mut ctx, &[10], 11);
    ctx.connect(50, 10);
    ctx.release_node(10);
    ctx.release_node(11);
    for block in 0..128 {
        let mut out = [0.0; 128];
        ctx.process(&mut out);
        let expected = if block == 0 {
            0.0
        } else {
            0.5f32.powi(block - 1)
        };
        assert_eq!(
            out[0].to_bits(),
            expected.to_bits(),
            "released cyclic Delay tail was truncated"
        );
        assert!(out[1..].iter().all(|&v| v == 0.0));
    }
    for _ in 0..3 {
        let mut out = [0.0; 128];
        ctx.process(&mut out);
        assert_eq!(
            out, [0.0; 128],
            "inactive subnormal feedback did not become silence"
        );
    }
    assert!(
        !ctx.nodes.contains_key(&10) && !ctx.nodes.contains_key(&11),
        "silent released feedback was not collected"
    );
}

// Independent sample recurrence, indexed by absolute source frame. It has no
// graph traversal, render-buffer reuse, Delay ring indexing or quantum state.
fn delayed(history: &[f32], frame: usize, channel: usize, channels: usize, delay: f32) -> f32 {
    let whole = delay.floor() as usize;
    let fraction = delay - whole as f32;
    let at = |age: usize| {
        frame
            .checked_sub(age)
            .map_or(0.0, |f| history[f * channels + channel])
    };
    let a = at(whole);
    let b = at(whole + 1);
    a + (b - a) * fraction
}
#[test]
fn complete_pcm_matches_sample_recurrence_across_orders_blocks_and_automation() {
    const BLOCKS: [usize; 8] = [1, 17, 127, 128, 129, 257, 511, 4097];
    let frames: usize = BLOCKS.iter().sum();
    for rate in [44100, 48000] {
        for channels in [1, 2, 32] {
            for order in 0..2 {
                for automated in [false, true] {
                    let mut ctx = AudioContext::new(1, rate, channels as u32);
                    let (source, delay, gain) = if order == 0 { (50, 10, 11) } else { (1, 90, 7) };
                    let input: Vec<f32> = (0..frames * channels)
                        .map(|i| {
                            ((i / channels) % 13 + 1) as f32 / 64.0 / (i % channels + 1) as f32
                        })
                        .collect();
                    let buffer = ctx
                        .create_empty_buffer(channels as u32, frames as u32, rate)
                        .unwrap();
                    for ch in 0..channels {
                        let samples: Vec<_> =
                            input.iter().skip(ch).step_by(channels).copied().collect();
                        ctx.copy_to_channel(buffer, &samples, ch as u32, 0).unwrap();
                    }
                    assert!(ctx.add_node(Box::new(DelayNode::new(
                        delay,
                        0.1,
                        rate,
                        channels as u32
                    ))));
                    assert!(ctx.create_gain(gain));
                    assert!(ctx.set_node_param(gain, "gain", 0.5));
                    assert!(ctx.create_buffer_source(source));
                    assert!(ctx.set_buffer(source, Some(buffer)));
                    assert!(ctx.start_source(source, 0.0, 0.0, None));
                    let seconds = if automated {
                        64.0 / rate as f32
                    } else {
                        128.5 / rate as f32
                    };
                    assert!(ctx.set_node_param(delay, "delayTime", seconds));
                    if automated {
                        assert!(ctx.param_linear_ramp(
                            delay,
                            "delayTime",
                            256.0 / rate as f32,
                            frames as f64 / rate as f64
                        ));
                    }
                    let connections = if order == 0 {
                        [(source, delay), (delay, gain), (gain, delay), (delay, 0)]
                    } else {
                        [(gain, delay), (delay, 0), (source, delay), (delay, gain)]
                    };
                    for (src, dst) in connections {
                        ctx.connect(src, dst);
                    }
                    let mut timeline = crate::param::AudioParamTimeline::new(seconds, 0.0, 0.1);
                    if automated {
                        timeline.linear_ramp_to_value_at_time(
                            256.0 / rate as f32,
                            frames as f64 / rate as f64,
                        );
                    }
                    let mut values = vec![0.0; frames];
                    timeline.compute_values(0.0, &mut values, rate);
                    let mut history = vec![0.0; input.len()];
                    let mut expected = vec![0.0; input.len()];
                    for (frame, value) in values.iter().enumerate() {
                        let d = (*value * rate as f32).max(128.0);
                        for ch in 0..channels {
                            let i = frame * channels + ch;
                            expected[i] = delayed(&history, frame, ch, channels, d);
                            history[i] = input[i] + 0.5 * expected[i];
                        }
                    }
                    let mut first = 0;
                    for count in BLOCKS {
                        let mut out = vec![0.0; count * channels];
                        ctx.process(&mut out);
                        assert!(
                            out.iter()
                                .zip(&expected[first * channels..(first + count) * channels])
                                .all(|(a, b)| a.to_bits() == b.to_bits()),
                            "feedback PCM differs from independent sample recurrence"
                        );
                        let bits: Vec<_> = out.iter().map(|v| v.to_bits()).collect();
                        println!(
                            "MIGO_FEEDBACK_PCM {rate}:{channels}:{order}:{automated}:{first}:{count} {:?} {}",
                            bits,
                            ctx.current_time().to_bits()
                        );
                        first += count;
                    }
                }
            }
        }
    }
}

/// Native equivalents of the official WPT processing-model cases, preserving
/// their topology, 48kHz rate, 512 frames and exact per-sample assertions.
/// Sources: https://github.com/web-platform-tests/wpt/tree/master/webaudio/the-audio-api/processing-model
/// cycle-without-delay.html, delay-time-clamping.html, feedback-delay-time.html.
/// Browser OfflineAudioContext/testharness itself is outside this native facet.
#[test]
fn wpt_processing_model_cycle_and_delay_cases() {
    for delay_frames in [None, Some(64), Some(128)] {
        let mut ctx = AudioContext::new(1, 48000, 1);
        assert!(ctx.create_gain(10));
        if let Some(frames) = delay_frames {
            impulse(&mut ctx, 50);
            assert!(ctx.add_node(Box::new(DelayNode::new(20, 1.0, 48000, 1))));
            assert!(ctx.set_node_param(20, "delayTime", frames as f32 / 48000.0));
            ctx.connect(50, 10);
            ctx.connect(10, 20);
            ctx.connect(20, 10);
        } else {
            let mut oscillator = crate::nodes::OscillatorNode::new(50, 48000);
            oscillator.start(0.0);
            assert!(ctx.add_node(Box::new(oscillator)));
            ctx.connect(50, 10);
            ctx.connect(10, 10);
        }
        ctx.connect(10, DESTINATION_NODE_ID);
        let mut out = [0.0; 512];
        ctx.process(&mut out);
        for (index, &sample) in out.iter().enumerate() {
            let expected = if delay_frames.is_some() && index % 128 == 0 {
                1.0
            } else {
                0.0
            };
            assert_eq!(
                sample, expected,
                "official WPT processing-model sample mismatch"
            );
        }
        println!(
            "MIGO_WPT_CYCLE {:?} {:?}",
            delay_frames,
            out.map(f32::to_bits)
        );
    }
}
