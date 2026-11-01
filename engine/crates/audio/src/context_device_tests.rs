use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn signal(frame: u64, channel: usize) -> f32 {
    ((frame * 17 + channel as u64 * 37) % 251) as f32 / 128.0 - 0.9
}
struct Ramp {
    frame: u64,
    largest: Arc<AtomicUsize>,
}
impl AudioNodeProcessor for Ramp {
    fn id(&self) -> AudioNodeId {
        10
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn is_producing(&self) -> bool {
        true
    }
    fn process(&mut self, _: &[f32], out: &mut [f32], _: u32, channels: u32, _: f64) -> usize {
        let frames = out.len() / channels as usize;
        self.largest.fetch_max(frames, Ordering::Relaxed);
        for (i, chunk) in out.chunks_exact_mut(channels as usize).enumerate() {
            for (channel, value) in chunk.iter_mut().enumerate() {
                *value = signal(self.frame + i as u64, channel);
            }
        }
        self.frame += frames as u64;
        frames
    }
}
fn ramp(rate: u32, channels: u32) -> (AudioContext, Arc<AtomicUsize>) {
    let mut context = AudioContext::new(1, rate, channels);
    let largest = Arc::new(AtomicUsize::new(0));
    context.add_node(Box::new(Ramp {
        frame: 0,
        largest: largest.clone(),
    }));
    context.connect(10, DESTINATION_NODE_ID);
    (context, largest)
}
fn prove_allocator() {
    let before = migo_alloc_probe::thread_counts();
    std::hint::black_box(vec![0_u8; 64]);
    assert!(
        migo_alloc_probe::thread_counts().allocations > before.allocations,
        "allocation probe is not installed"
    );
}
#[test]
fn native_clock_and_pcm_are_partition_independent() {
    for rate in [3000, 44100, 48000, 768000] {
        let (mut ctx, largest) = ramp(rate, 1);
        let mut position = 0;
        for frames in [1usize, 3, 127, 128, 511].into_iter().cycle().take(25) {
            let mut out = vec![0.0; frames];
            ctx.process_native_device(&mut out, rate, 1).unwrap();
            assert_eq!(
                ctx.frames_processed,
                position + frames as u64,
                "native graph clock drifted"
            );
            for (i, &sample) in out.iter().enumerate() {
                assert_eq!(
                    sample,
                    signal(position + i as u64, 0),
                    "native PCM skipped or repeated a frame"
                );
            }
            position += frames as u64;
        }
        assert!(
            largest.load(Ordering::Relaxed) <= 128,
            "native source render exceeded one quantum"
        );
    }
}
#[test]
fn native_channel_mapping_preserves_the_additive_mix_and_exact_clock() {
    for (source, target) in [(1, 2), (2, 1), (1, 32), (32, 1), (3, 2), (2, 4), (32, 32)] {
        let (mut ctx, largest) = ramp(48000, source);
        let mut out = vec![0.25; 257 * target as usize];
        ctx.process_native_device(&mut out, 48000, target).unwrap();
        assert_eq!(
            ctx.frames_processed, 257,
            "device layout changed native duration"
        );
        for (frame, actual) in out.chunks_exact(target as usize).enumerate() {
            let src: Vec<_> = (0..source as usize)
                .map(|ch| signal(frame as u64, ch))
                .collect();
            let mut expected = vec![0.25; target as usize];
            if source == 1 {
                expected.fill(0.25 + src[0]);
            } else if target == 1 {
                expected[0] += src.iter().sum::<f32>() / source as f32;
            } else {
                for (to, from) in expected.iter_mut().zip(src) {
                    *to += from;
                }
            }
            assert_eq!(
                actual, expected,
                "native channel mapping changed the shared mix"
            );
        }
        assert!(
            largest.load(Ordering::Relaxed) <= 128,
            "native source render exceeded one quantum"
        );
    }
}
#[test]
fn native_rate_mismatch_is_rejected_without_graph_bus_or_epoch_changes() {
    for (source, target) in [
        (44100, 48000),
        (48000, 44100),
        (3000, 768000),
        (768000, 3000),
    ] {
        let (mut ctx, largest) = ramp(source, 2);
        let epoch = ctx.device_epoch();
        let mut out = [0.25; 256];
        let error = ctx
            .process_native_device(&mut out, target, 2)
            .expect_err("native rate mismatch was accepted");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(out, [0.25; 256], "wrong-rate native call modified the bus");
        assert_eq!(ctx.frames_processed, 0);
        assert_eq!(largest.load(Ordering::Relaxed), 0);
        assert!(epoch.is_live() && epoch.same(&ctx.device_epoch()));
    }
}
#[test]
fn native_suspend_empty_and_partial_calls_preserve_clock_and_mix() {
    let (mut ctx, _) = ramp(44100, 1);
    ctx.process_native_device(&mut [0.0; 256], 44100, 2)
        .unwrap();
    let before = ctx.frames_processed;
    ctx.suspend();
    let mut out = [0.25; 256];
    assert!(
        ctx.process_native_device(&mut out, 44100, 2)
            .unwrap()
            .is_empty()
    );
    assert_eq!(out, [0.25; 256], "native suspension modified the mix");
    assert_eq!(ctx.state, AudioContextState::Suspended);
    assert_eq!(ctx.frames_processed, before);
    ctx.resume();
    ctx.process_native_device(&mut [], 44100, 2).unwrap();
    let mut partial = [0.25];
    ctx.process_native_device(&mut partial, 44100, 2).unwrap();
    assert_eq!(partial, [0.25], "partial native frame changed the mix");
    assert_eq!(ctx.frames_processed, before);
    out.fill(0.0);
    ctx.process_native_device(&mut out, 44100, 2).unwrap();
    assert_eq!(out[0], signal(before, 0));
}
fn finite(ctx: &mut AudioContext, id: u32, frames: usize) {
    let buffer = ctx
        .add_buffer(DecodedAudio {
            samples: vec![0.25; frames],
            sample_rate: ctx.sample_rate,
            channels: 1,
        })
        .unwrap();
    ctx.create_buffer_source(id);
    ctx.set_buffer(id, Some(buffer));
    ctx.connect(id, DESTINATION_NODE_ID);
    ctx.watch_source_ended(id, true);
}
#[test]
fn native_collects_all_subquantum_nodes_and_ended_once() {
    let mut ctx = AudioContext::new(1, 96000, 1);
    for (id, n) in [(10, 1), (11, 129)] {
        finite(&mut ctx, id, n);
        ctx.start_source(id, 0.0, 0.0, None);
    }
    let mut out = [0.125; 257];
    let mut removed = ctx
        .process_native_device(&mut out, 96000, 1)
        .unwrap()
        .to_vec();
    removed.sort_unstable();
    assert_eq!(
        removed,
        [10, 11],
        "earlier native subquantum node ids were lost"
    );
    let mut ended: Vec<_> = ctx.drain_ended().collect();
    ended.sort_unstable();
    assert_eq!(ended, [10, 11]);
    assert_eq!(ctx.frames_processed, 256);
    assert_eq!(out[0], 0.625);
    assert_eq!(out[128], 0.375);
    assert_eq!(out[256], 0.125);
    assert!(
        ctx.process_native_device(&mut out, 96000, 1)
            .unwrap()
            .is_empty(),
        "native collection was repeated"
    );
    assert_eq!(ctx.drain_ended().count(), 0);
}
#[test]
fn native_32_channel_scratch_is_one_quantum_without_a_right_neighbour() {
    for (source, target) in [(32, 1), (1, 32), (32, 32)] {
        let (mut ctx, largest) = ramp(768000, source);
        let mut out = vec![0.0; 511 * target as usize];
        ctx.process_native_device(&mut out, 768000, target).unwrap();
        assert_eq!(ctx.frames_processed, 511);
        assert!(
            largest.load(Ordering::Relaxed) <= 128,
            "native source render exceeded one quantum"
        );
        assert!(ctx.native_input.is_empty());
        assert_eq!(
            ctx.native_input.capacity(),
            128 * source as usize,
            "native scratch retained interpolation storage"
        );
    }
}
#[test]
fn native_warmed_channel_layouts_do_not_allocate() {
    prove_allocator();
    for (source, target) in [(1, 2), (2, 1), (32, 32), (32, 2)] {
        let (mut ctx, _) = ramp(44100, source);
        let mut out = vec![0.0; 257 * target as usize];
        for _ in 0..4 {
            ctx.process_native_device(&mut out, 44100, target).unwrap();
        }
        for _ in 0..16 {
            let before = migo_alloc_probe::thread_counts();
            ctx.process_native_device(&mut out, 44100, target).unwrap();
            let after = migo_alloc_probe::thread_counts();
            assert_eq!(
                (after.allocations, after.reallocations),
                (before.allocations, before.reallocations),
                "native callback allocated"
            );
        }
    }
}
#[test]
fn native_stop_burst_uses_command_reserved_collectors_without_allocation() {
    prove_allocator();
    let mut ctx = AudioContext::new(1, 48000, 1);
    for id in 10..42 {
        finite(&mut ctx, id, 1);
    }
    ctx.process(&mut [0.0; 128]);
    let now = ctx.current_time();
    for id in 10..42 {
        ctx.start_source(id, now, 0.0, None);
    }
    let before = migo_alloc_probe::thread_counts();
    let ids = ctx
        .process_native_device(&mut [0.0; 257], 48000, 1)
        .unwrap();
    assert_eq!(ids.len(), 32);
    for id in 10..42 {
        assert!(ids.contains(&id));
    }
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        (after.allocations, after.reallocations),
        (before.allocations, before.reallocations),
        "native stop burst allocated"
    );
    assert_eq!(ctx.drain_ended().count(), 32);
    assert_eq!(ctx.frames_processed, 256);
}
#[test]
fn native_route_replacement_clears_only_scratch_and_replaces_capability() {
    let (mut ctx, _) = ramp(3000, 1);
    ctx.process_native_device(&mut [0.0; 1], 3000, 1).unwrap();
    let before = ctx.frames_processed;
    let old = ctx.device_epoch();
    ctx.native_input.push(99.0);
    ctx.native_collected.push(88);
    ctx.renegotiate_device(3000, 2);
    assert!(!old.is_live());
    assert!(
        ctx.native_input.is_empty() && ctx.native_collected.is_empty(),
        "native route replacement retained staging"
    );
    let next = ctx.device_epoch();
    let mut out = [0.0; 256];
    ctx.process_native_device(&mut out, 3000, 2).unwrap();
    assert_eq!(out[0], signal(before, 0));
    assert_eq!(out[1], out[0]);
    assert_eq!(ctx.frames_processed, before + 128);
    assert!(next.is_live() && next.same(&ctx.device_epoch()));
}
#[test]
fn inactive_native_graph_has_no_node_dsp_or_clock_work() {
    struct Quiet(Arc<AtomicUsize>);
    impl AudioNodeProcessor for Quiet {
        fn id(&self) -> AudioNodeId {
            10
        }
        fn is_producing(&self) -> bool {
            false
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn process(&mut self, _: &[f32], out: &mut [f32], _: u32, _: u32, _: f64) -> usize {
            self.0.fetch_add(1, Ordering::Relaxed);
            out.fill(1e-12);
            out.len()
        }
    }
    let mut ctx = AudioContext::new(1, 48000, 1);
    let calls = Arc::new(AtomicUsize::new(0));
    ctx.add_node(Box::new(Quiet(calls.clone())));
    ctx.connect(10, DESTINATION_NODE_ID);
    let mut out = [0.0; 257];
    ctx.process_native_device(&mut out, 48000, 1).unwrap();
    assert_eq!(
        calls.load(Ordering::Relaxed),
        0,
        "inactive native graph executed node DSP"
    );
    assert_eq!(ctx.frames_processed, 0);
    assert_eq!(out, [0.0; 257]);
}
