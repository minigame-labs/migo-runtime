use super::*;
use crate::{
    context::DESTINATION_NODE_ID,
    decoder::DecodedAudio,
    nodes::{AudioNodeProcessor, BiquadFilterNode, BiquadFilterType, DelayNode},
};
use std::{
    sync::atomic::AtomicUsize,
    thread,
    time::{Duration, Instant},
};

fn fixture(
    rate: u32,
    target: u32,
    channels: u32,
) -> (
    migo_io::scheduler::IoScheduler,
    AudioContext,
    Controller,
    Device,
) {
    let scheduler = migo_io::scheduler::IoScheduler::new(0);
    let context = AudioContext::new(1, rate, channels);
    let mut controller = Controller {
        host: Host {
            cpu: scheduler.audio_cpu().background(),
            usage: Usage::new(8),
            process: Usage::new(8),
            aggregate: Arc::new(AudioAggregateLedger::new(8 * OWNER_BYTES)),
            retire_hook: None,
        },
        slots: HashMap::new(),
    };
    controller
        .ensure(&context, target, ThreadWakeup::new(), None)
        .unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    let device = loop {
        if let Some(d) = controller.take(&context) {
            break d.unwrap();
        }
        assert!(Instant::now() < end);
        thread::yield_now();
    };
    (scheduler, context, controller, device)
}
fn source(context: &mut AudioContext, id: u32, frames: usize, amplitude: f32) {
    let channels = context.channels();
    let buffer = context
        .add_buffer(DecodedAudio {
            sample_rate: context.sample_rate(),
            channels,
            samples: (0..frames * channels as usize)
                .map(|n| amplitude * ((n * 17 % 127) as f32 / 64.0 - 0.5))
                .collect(),
        })
        .unwrap();
    context.create_buffer_source(id);
    assert!(context.set_buffer(id, Some(buffer)));
    context.connect(id, DESTINATION_NODE_ID);
    context.watch_source_ended(id, true);
    context.start_source(id, context.current_time(), 0.0, None);
}
type Capture = (Vec<f32>, Vec<f32>, Vec<(u64, u32)>, Vec<u32>);

fn drain(device: &mut Device, context: &mut AudioContext, partition: usize) -> Capture {
    let channels = context.channels() as usize;
    let mut out = vec![0.0; partition * channels];
    let mut pcm = Vec::new();
    let mut input = Vec::new();
    let mut events = Vec::new();
    let mut collected = Vec::new();
    for _ in 0..200000 {
        out.fill(0.0);
        let turn = device
            .step_context(context, &mut out, |id| collected.push(id))
            .unwrap_or_else(|error| panic!("real graph step failed: {error:?}"));
        if let SessionTurn::Work(Turn::Rendered(n)) = turn {
            let b = device.session().unwrap().pull.buffers.as_ref().unwrap();
            input.extend_from_slice(&b.input[..n * channels]);
        }
        if let SessionTurn::Work(Turn::Published(n)) = turn {
            pcm.extend_from_slice(&out[..n * channels]);
        }
        while let Some(id) = device.pop_ended_context(context).unwrap() {
            events.push((device.graph_clocks(context).unwrap().published_output, id));
        }
        if turn == SessionTurn::Quiescent {
            return (pcm, input, events, collected);
        }
    }
    panic!("real context never became quiescent");
}
fn reference(input: &[f32], rate: u32, target: u32, channels: u32) -> Vec<f32> {
    let mut samples = input.to_vec();
    samples.resize(samples.len() + 4096 * channels as usize, 0.0);
    crate::resample_job::tests::reference(
        &DecodedAudio {
            sample_rate: rate,
            channels,
            samples,
        },
        target,
    )
}
fn effects(context: &mut AudioContext) {
    let rate = context.sample_rate();
    let channels = context.channels();
    let buffer = context
        .add_buffer(DecodedAudio {
            sample_rate: rate,
            channels,
            samples: vec![1.0; channels as usize],
        })
        .unwrap();
    context.create_buffer_source(10);
    context.set_buffer(10, Some(buffer));
    context.watch_source_ended(10, true);
    context.start_source(10, 0.0, 0.0, None);
    let mut delay = DelayNode::new(20, 0.02, rate, channels);
    delay.get_param_mut("delayTime").unwrap().set_value(0.01);
    context.add_node(Box::new(delay));
    let mut biquad = BiquadFilterNode::new(21, channels, rate);
    biquad.set_type(BiquadFilterType::Lowpass);
    biquad.get_param_mut("frequency").unwrap().set_value(440.0);
    biquad.get_param_mut("Q").unwrap().set_value(4.0);
    context.add_node(Box::new(biquad));
    context.connect(10, 20);
    context.connect(20, 21);
    context.connect(21, DESTINATION_NODE_ID);
}
#[test]
fn inactive_source_port_is_exact_zero_and_does_not_process_nodes() {
    struct Quiet(Arc<AtomicUsize>);
    impl AudioNodeProcessor for Quiet {
        fn id(&self) -> u32 {
            7
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
    let mut context = AudioContext::new(1, 44100, 1);
    let calls = Arc::new(AtomicUsize::new(0));
    context.add_node(Box::new(Quiet(calls.clone())));
    context.connect(7, DESTINATION_NODE_ID);
    let mut out = [0.25; 128];
    context.process_device_source(&mut out);
    assert!(
        out.iter().all(|&x| x == 0.0),
        "inactive graph port leaked nonzero state"
    );
    assert_eq!(
        calls.load(Ordering::Acquire),
        0,
        "inactive graph port ran node DSP"
    );
    assert_eq!(context.device_graph_frame(), 128);
}
#[test]
fn real_delay_and_biquad_tails_match_complete_mature_reference_pcm() {
    for (rate, target, channels) in [(44100, 48000, 1), (48000, 44100, 2), (96000, 8000, 2)] {
        let (_s, mut ctx, _controller, mut device) = fixture(rate, target, channels);
        effects(&mut ctx);
        let (pcm, input, events, collected) = drain(&mut device, &mut ctx, 127);
        let expected = reference(&input, rate, target, channels);
        assert!(
            pcm.iter().any(|x| x.abs() > 1e-4),
            "real effect graph produced no audible tail"
        );
        assert_eq!(
            pcm,
            expected[..pcm.len()],
            "real graph adapter changed complete mature PCM"
        );
        assert!(
            expected[pcm.len()..].iter().all(|&x| x == 0.0),
            "real graph quiesced before its mature filter tail"
        );
        let block = 128_u64;
        assert_eq!(
            events,
            [(block * (target as u64)).div_ceil(rate as u64)]
                .into_iter()
                .map(|f| (f, 10))
                .collect::<Vec<_>>()
        );
        assert_eq!(collected, [10]);
        assert!(!ctx.has_active_sources());
        let delay_frame = (0.01 * target as f64) as usize;
        assert!(pcm.len() / channels as usize > delay_frame);
        let tail_energy: f32 = pcm[delay_frame.saturating_sub(4) * channels as usize..]
            .iter()
            .map(|x| x * x)
            .sum();
        assert!(
            tail_energy > 1e-5,
            "effect tail was lost after source collection"
        );
    }
}
#[test]
fn quiescent_context_has_no_source_kernel_heap_or_clock_work() {
    let (_s, mut ctx, _c, mut d) = fixture(44100, 48000, 2);
    source(&mut ctx, 10, 129, 0.5);
    drain(&mut d, &mut ctx, 128);
    let before_clock = d.graph_clocks(&ctx).unwrap();
    let mut out = [0.25; 256];
    bandlimited::take_trace();
    let before = migo_alloc_probe::thread_counts();
    for _ in 0..1000 {
        assert_eq!(
            d.step_context(&mut ctx, &mut out, |_| {}).unwrap(),
            SessionTurn::Quiescent,
            "idle context kept doing SRC work"
        );
        assert!(d.pop_ended_context(&ctx).unwrap().is_none());
    }
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        before.allocations, after.allocations,
        "idle graph adapter allocated"
    );
    assert_eq!(before.reallocations, after.reallocations);
    assert_eq!(bandlimited::take_trace().calls, 0);
    assert_eq!(
        d.graph_clocks(&ctx).unwrap(),
        before_clock,
        "idle adapter advanced local clocks"
    );
    assert_eq!(out, [0.25; 256]);
}
#[test]
fn immediate_stop_is_collected_and_ended_at_zero_fence_without_rendering() {
    let (_s, mut ctx, _c, mut d) = fixture(44100, 48000, 2);
    ctx.create_buffer_source(10);
    ctx.watch_source_ended(10, true);
    ctx.stop_source(10, 0.0);
    let mut out = [0.25; 256];
    let mut collected = Vec::new();
    bandlimited::take_trace();
    assert_eq!(
        d.step_context(&mut ctx, &mut out, |id| collected.push(id))
            .unwrap(),
        SessionTurn::Work(Turn::EventsReady),
        "idle control event was hidden behind quiescence"
    );
    assert_eq!(collected, [10]);
    assert_eq!(
        ctx.device_graph_frame(),
        0,
        "control sweep rendered a silent block"
    );
    assert_eq!(bandlimited::take_trace().calls, 0);
    assert_eq!(d.pop_ended_context(&ctx).unwrap(), Some(10));
    assert_eq!(d.pop_ended_context(&ctx).unwrap(), None);
    assert_eq!(
        d.step_context(&mut ctx, &mut out, |_| {}).unwrap(),
        SessionTurn::Quiescent
    );
    assert_eq!(out, [0.25; 256]);
}
#[test]
fn natural_end_collection_is_immediate_but_notifications_wait_for_local_fences() {
    for (rate, target) in [(96000, 3000), (44100, 48000), (48000, 44100)] {
        let (_s, mut ctx, _c, mut d) = fixture(rate, target, 1);
        ctx.set_test_device_wall_epoch(1.0);
        source(&mut ctx, 10, 1, 0.25);
        source(&mut ctx, 11, 129, 0.5);
        source(&mut ctx, 12, 128, 0.25);
        source(&mut ctx, 13, 256, 0.25);
        let buffer = ctx
            .add_buffer(DecodedAudio {
                samples: vec![0.25; 4],
                sample_rate: rate,
                channels: 1,
            })
            .unwrap();
        let mut looped = crate::nodes::BufferSourceNode::new(14, rate);
        looped.set_buffer(ctx.get_buffer(buffer));
        looped.set_loop(true, 0.0, 0.0);
        looped.start(
            ctx.current_time(),
            0.0,
            Some(128.0 / rate as f64),
            ctx.current_time(),
        );
        ctx.add_node(Box::new(looped));
        ctx.connect(14, DESTINATION_NODE_ID);
        ctx.watch_source_ended(14, true);
        ctx.create_buffer_source(15);
        ctx.connect(15, DESTINATION_NODE_ID);
        ctx.watch_source_ended(15, true);
        ctx.start_source(15, ctx.current_time(), 0.0, None);
        let mut out = [0.0; 128];
        let mut collected = Vec::new();
        assert_eq!(
            d.step_context(&mut ctx, &mut out, |id| collected.push(id))
                .unwrap(),
            SessionTurn::Work(Turn::Rendered(128))
        );
        collected.sort_unstable();
        assert_eq!(
            collected,
            [10, 12, 14, 15],
            "natural source collection missed its final SRC input block"
        );
        assert!(
            d.pop_ended_context(&ctx).unwrap().is_none(),
            "ended escaped during graph lookahead"
        );
        let (_, _, mut events, mut later) = drain(&mut d, &mut ctx, 128);
        events.sort_unstable();
        later.sort_unstable();
        let first = (128_u64 * target as u64).div_ceil(rate as u64);
        let second = (256_u64 * target as u64).div_ceil(rate as u64);
        let mut expected = vec![
            (first, 10),
            (first, 12),
            (first, 14),
            (first, 15),
            (second, 11),
            (second, 13),
        ];
        expected.sort_unstable();
        assert_eq!(
            events, expected,
            "ended used absolute graph time or crossed its local fence"
        );
        assert_eq!(later, [11, 13]);
        let clocks = d.graph_clocks(&ctx).unwrap();
        assert!(clocks.absolute_graph_frame >= rate as u64 + clocks.stream_source);
        assert!(clocks.public_seconds >= 1.0);
        assert_eq!(
            clocks.published_source,
            (clocks.published_output * rate as u64) / target as u64
        );
    }
}
#[test]
fn suspend_partial_and_empty_calls_freeze_graph_controls_and_publication() {
    let (_s, mut ctx, _c, mut d) = fixture(44100, 48000, 2);
    source(&mut ctx, 10, 513, 0.5);
    let mut out = [0.25; 256];
    d.step_context(&mut ctx, &mut out, |_| {}).unwrap();
    ctx.suspend();
    ctx.stop_source(10, 0.0);
    let before = d.graph_clocks(&ctx).unwrap();
    let mut collected = 0;
    for _ in 0..4 {
        assert_eq!(
            d.step_context(&mut ctx, &mut out, |_| collected += 1)
                .unwrap(),
            SessionTurn::Work(Turn::Idle),
            "suspended graph adapter advanced a turn"
        );
    }
    assert_eq!(collected, 0, "suspension serviced graph controls");
    assert!(d.pop_ended_context(&ctx).unwrap().is_none());
    assert_eq!(d.graph_clocks(&ctx).unwrap(), before);
    ctx.resume();
    let before = d.graph_clocks(&ctx).unwrap();
    for n in [0, 1] {
        assert_eq!(
            d.step_context(&mut ctx, &mut out[..n], |_| collected += 1)
                .unwrap(),
            SessionTurn::Work(Turn::Idle)
        );
    }
    assert_eq!(
        collected, 0,
        "empty/partial callback serviced graph controls"
    );
    assert_eq!(d.graph_clocks(&ctx).unwrap(), before);
    let (_, _, events, collected) = drain(&mut d, &mut ctx, 128);
    assert_eq!(collected, [10]);
    assert_eq!(events.len(), 1);
}
#[test]
fn control_event_overflow_is_terminal_without_partial_pcm_or_duplicate_collection() {
    let (_s, mut ctx, _c, mut d) = fixture(44100, 48000, 2);
    d.session().unwrap().pull.limits.pending_events = 1;
    for id in [10, 11] {
        ctx.create_buffer_source(id);
        ctx.watch_source_ended(id, true);
        ctx.stop_source(id, 0.0);
    }
    let mut collected = Vec::new();
    let mut out = [0.25; 256];
    assert_eq!(
        d.step_context(&mut ctx, &mut out, |id| collected.push(id))
            .err()
            .map(|e| e.code),
        Some(ErrorCode::InputSaturated),
        "control events bypassed the pending-event budget"
    );
    collected.sort();
    assert_eq!(collected, [10, 11]);
    assert_eq!(out, [0.25; 256]);
    assert!(d.lease.is_none());
    assert_eq!(
        d.pop_ended_context(&ctx).err().map(|e| e.code),
        Some(ErrorCode::InputSaturated)
    );
}
#[test]
fn graph_wall_stream_and_mix_clocks_remain_separate_across_idle_reactivation() {
    let (_s, mut ctx, _c, mut d) = fixture(48000, 44100, 2);
    ctx.set_test_device_wall_epoch(1.0);
    source(&mut ctx, 10, 129, 0.5);
    let (pcm, mut input, _, _) = drain(&mut d, &mut ctx, 128);
    let first = d.graph_clocks(&ctx).unwrap();
    assert!(first.absolute_graph_frame >= 48000 + first.stream_source);
    ctx.set_test_device_wall_epoch(10.0);
    let idle = d.graph_clocks(&ctx).unwrap();
    assert_eq!(idle.absolute_graph_frame, first.absolute_graph_frame);
    assert_eq!(idle.stream_source, first.stream_source);
    assert_eq!(
        idle.public_seconds, 10.0,
        "public idle clock was replaced by the local SRC clock"
    );
    source(&mut ctx, 11, 257, 0.5);
    let (second, next, _, _) = drain(&mut d, &mut ctx, 3);
    input.extend(next);
    let mut all = pcm;
    all.extend(second);
    let expected = reference(&input, 48000, 44100, 2);
    assert_eq!(
        all,
        expected[..all.len()],
        "absolute idle epoch reset the continuous SRC phase"
    );
    let end = d.graph_clocks(&ctx).unwrap();
    assert!(end.absolute_graph_frame >= 480000);
    assert!(
        end.stream_source < 480000,
        "wall gap was submitted as silent SRC input"
    );
    assert_eq!(end.published_source, end.published_output * 48000 / 44100);
}
#[test]
fn foreign_context_and_revoked_epoch_are_rejected_before_graph_or_cached_output() {
    let (_s, mut ctx, _c, mut d) = fixture(44100, 48000, 2);
    let mut foreign = AudioContext::new(1, 44100, 2);
    let mut out = [0.25; 256];
    let mut collected = 0;
    assert_eq!(
        d.step_context(&mut foreign, &mut out, |_| collected += 1)
            .err()
            .map(|e| e.code),
        Some(ErrorCode::Cancelled),
        "graph adapter rendered a foreign context"
    );
    assert_eq!(foreign.device_graph_frame(), 0);
    assert_eq!(collected, 0);
    assert_eq!(out, [0.25; 256]);
    let (_s2, mut ctx2, _c2, mut d2) = fixture(44100, 48000, 2);
    ctx2.renegotiate_device(48000, 2);
    assert_eq!(
        d2.step_context(&mut ctx2, &mut out, |_| {})
            .err()
            .map(|e| e.code),
        Some(ErrorCode::Cancelled)
    );
    ctx.close();
}
#[test]
fn warmed_graph_first_and_steady_turns_including_watched_end_allocate_nothing() {
    let (_s, mut ctx, _c, mut d) = fixture(44100, 48000, 2);
    source(&mut ctx, 10, 1024, 0.5);
    ctx.process(&mut [0.0; 256]);
    let mut out = [0.0; 256];
    let mut ended = 0;
    let mut done = false;
    for _ in 0..10000 {
        bandlimited::take_trace();
        let before = migo_alloc_probe::thread_counts();
        let turn = d.step_context(&mut ctx, &mut out, |_| {}).unwrap();
        while d.pop_ended_context(&ctx).unwrap().is_some() {
            ended += 1;
        }
        let after = migo_alloc_probe::thread_counts();
        assert_eq!(
            after.allocations, before.allocations,
            "graph turn or watched end allocated"
        );
        assert_eq!(
            after.reallocations, before.reallocations,
            "graph turn or watched end reallocated"
        );
        let trace = bandlimited::take_trace();
        assert!(trace.calls <= 1 && trace.input_samples <= 8192 && trace.output_samples <= 8192);
        if turn == SessionTurn::Quiescent {
            done = true;
            break;
        }
    }
    assert!(done && ended == 1);
}
#[test]
fn native_source_port_does_not_resurrect_subthreshold_biquad_state() {
    let mut ctx = AudioContext::new(1, 44100, 1);
    let buffer = ctx
        .add_buffer(DecodedAudio {
            sample_rate: 44100,
            channels: 1,
            samples: vec![1e-12],
        })
        .unwrap();
    ctx.create_buffer_source(10);
    ctx.set_buffer(10, Some(buffer));
    ctx.start_source(10, 0.0, 0.0, None);
    let mut filter = BiquadFilterNode::new(20, 1, 44100);
    filter.get_param_mut("frequency").unwrap().set_value(440.0);
    ctx.add_node(Box::new(filter));
    ctx.connect(10, 20);
    ctx.connect(20, DESTINATION_NODE_ID);
    let mut first = [0.0; 128];
    ctx.process(&mut first);
    assert!(first.iter().any(|x| *x != 0.0));
    assert!(!ctx.has_active_sources());
    let mut output = [0.0; 128];
    ctx.process_native_device(&mut output, 44100, 1).unwrap();
    assert!(
        output.iter().all(|&x| x == 0.0),
        "native source bypassed inactive graph policy"
    );
}
#[test]
fn a_new_control_fence_wakes_a_quiescent_stream_without_rebuilding_it() {
    let (_s, mut ctx, _c, mut d) = fixture(44100, 48000, 2);
    source(&mut ctx, 10, 129, 0.5);
    drain(&mut d, &mut ctx, 128);
    let before = d.graph_clocks(&ctx).unwrap();
    let ready_ptr = &*d.session().unwrap().pull.ready as *const Ready as usize;
    let fence = (before.stream_source * 48000).div_ceil(44100);
    assert!(
        fence > before.published_output,
        "fixture has no staged silent suffix"
    );
    ctx.create_buffer_source(11);
    ctx.watch_source_ended(11, true);
    ctx.stop_source(11, 0.0);
    let mut output = [0.0; 256];
    let mut event = None;
    let mut collected = Vec::new();
    for _ in 0..1000 {
        d.step_context(&mut ctx, &mut output, |id| collected.push(id))
            .unwrap();
        if let Some(id) = d.pop_ended_context(&ctx).unwrap() {
            event = Some((d.graph_clocks(&ctx).unwrap().published_output, id));
            break;
        }
    }
    assert_eq!(
        event,
        Some((fence, 11)),
        "quiescent stream did not flush pending control fence"
    );
    assert_eq!(collected, [11]);
    assert_eq!(
        &*d.session().unwrap().pull.ready as *const Ready as usize,
        ready_ptr
    );
}
