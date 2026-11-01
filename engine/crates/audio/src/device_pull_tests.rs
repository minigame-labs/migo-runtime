use super::*;
use crate::{
    decoder::DecodedAudio,
    device_prepare::{Format, Slot},
};
use migo_io::scheduler::IoScheduler;
use shared::channel::ThreadWakeup;
use std::time::{Duration, Instant};

fn fmt(source: u32, target: u32, channels: u32) -> Format {
    Format {
        source,
        target,
        channels,
    }
}
fn limits() -> Limits {
    Limits {
        source_ahead: MAX_AHEAD,
        pending_events: 64,
    }
}
fn ready(host: &IoScheduler, f: Format) -> (Slot, Box<Ready>) {
    let mut slot = Slot::new(1);
    slot.request(f, host.audio_cpu(), ThreadWakeup::new(), None)
        .unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(result) = slot.take() {
            return (slot, result.unwrap());
        }
        assert!(
            Instant::now() < end,
            "device pull preparation did not finish"
        );
        std::thread::yield_now();
    }
}
fn pull(host: &IoScheduler, f: Format, budget: Limits) -> (Slot, Box<Pull>) {
    let (slot, ready) = ready(host, f);
    let owner = crate::streaming::get_stream_runtime()
        .block_on(Pull::new(ready, budget))
        .unwrap();
    (slot, owner)
}
fn audio(f: Format, frames: usize) -> DecodedAudio {
    DecodedAudio {
        sample_rate: f.source,
        channels: f.channels,
        samples: (0..frames * f.channels as usize)
            .map(|n| ((n * 17) % 251) as f32 / 128.0 - 0.9)
            .collect(),
    }
}
struct Samples {
    audio: DecodedAudio,
    at: usize,
    calls: usize,
    largest: usize,
    notices: VecDeque<(u64, u32)>,
    ended: Vec<u32>,
}
impl Samples {
    fn new(audio: DecodedAudio) -> Self {
        Self {
            audio,
            at: 0,
            calls: 0,
            largest: 0,
            notices: VecDeque::new(),
            ended: Vec::with_capacity(64),
        }
    }
    fn notice(&mut self, frame: u64, id: u32) {
        self.notices.push_back((frame, id));
    }
}
impl Source for Samples {
    fn render(&mut self, out: &mut [f32]) -> EngineResult<Block> {
        self.calls += 1;
        self.largest = self.largest.max(out.len() / self.audio.channels as usize);
        let n = out.len().min(self.audio.samples.len() - self.at);
        for (to, from) in out[..n]
            .iter_mut()
            .zip(&self.audio.samples[self.at..self.at + n])
        {
            *to += *from;
        }
        self.at += n;
        let frame = self.at / self.audio.channels as usize;
        while self
            .notices
            .front()
            .is_some_and(|&(end, _)| end <= frame as u64)
        {
            self.ended.push(self.notices.pop_front().unwrap().1);
        }
        Ok(Block {
            frames: n / self.audio.channels as usize,
            eof: self.at == self.audio.samples.len(),
        })
    }
    fn drain_ended(&mut self) -> impl Iterator<Item = u32> {
        self.ended.drain(..)
    }
}
fn collect(
    owner: &mut Pull,
    source: &mut impl Source,
    partition: usize,
) -> (Vec<f32>, Vec<(u64, u32)>) {
    collect_with_base(owner, source, partition, 0.0)
}
fn collect_with_base(
    owner: &mut Pull,
    source: &mut impl Source,
    partition: usize,
    base: f32,
) -> (Vec<f32>, Vec<(u64, u32)>) {
    let channels = owner.ready.request.format.channels as usize;
    let mut output = vec![0.0; partition * channels];
    let mut pcm = Vec::new();
    let mut events = Vec::new();
    for _ in 0..200000 {
        output.fill(base);
        let turn = owner.step(source, &mut output).unwrap();
        if let Turn::Published(frames) = turn {
            pcm.extend_from_slice(&output[..frames * channels]);
        }
        while let Some(id) = owner.pop_ended().unwrap() {
            events.push((owner.published, id));
        }
        if turn == Turn::Finished {
            return (pcm, events);
        }
    }
    panic!("device pull never reached its exact endpoint");
}
fn fifo(owner: &mut Pull, source: &mut impl Source) {
    let mut out = vec![0.0; 128 * owner.ready.request.format.channels as usize];
    for _ in 0..100000 {
        assert!(!matches!(
            owner.step(source, &mut out).unwrap(),
            Turn::Finished | Turn::EventsReady
        ));
        if owner.output_head < owner.ready.output.len() {
            return;
        }
    }
    panic!("device pull produced no cached PCM");
}

#[test]
fn complete_pcm_and_ceil_endpoint_survive_varied_output_partitions() {
    let host = IoScheduler::new(0);
    for f in [
        fmt(44100, 48000, 1),
        fmt(48000, 44100, 2),
        fmt(96000, 8000, 2),
        fmt(768000, 3000, 32),
        fmt(3000, 768000, 32),
        fmt(3000, 768000, 2),
    ] {
        let frames = if f.target > 4 * f.source && f.channels == 32 {
            17
        } else {
            513
        };
        let reference = crate::resample_job::tests::reference(&audio(f, frames), f.target);
        for partition in [1, 3, 127, 128, 511] {
            let (_route, mut owner) = pull(&host, f, limits());
            let mut source = Samples::new(audio(f, frames));
            source.notice(frames as u64, 7);
            let base = if partition == 511 { 0.25 } else { 0.0 };
            let (pcm, events) = collect_with_base(&mut owner, &mut source, partition, base);
            let expected: Vec<_> = reference.iter().map(|&sample| base + sample).collect();
            assert_eq!(
                pcm, expected,
                "device FIFO changed mature PCM or lost an input suffix"
            );
            let end = (frames as u64 * f.target as u64).div_ceil(f.source as u64);
            assert_eq!(
                owner.clocks(),
                Clocks {
                    rendered_source: frames as u64,
                    published_output: end,
                    published_source: frames as u64
                },
                "device FIFO clock drifted with callback partitioning"
            );
            assert_eq!(events, [(end, 7)], "EOF event lost its original ceil fence");
        }
    }
}

#[test]
fn each_turn_obeys_one_source_or_kernel_or_bounded_publication() {
    let host = IoScheduler::new(0);
    for f in [
        fmt(44100, 48000, 2),
        fmt(768000, 3000, 32),
        fmt(3000, 768000, 32),
    ] {
        let (_route, mut owner) = pull(&host, f, limits());
        let mut source = Samples::new(audio(f, 513));
        let mut out = vec![0.0; 1024 * f.channels as usize];
        let mut kinds = [0; 3];
        for _ in 0..100000 {
            let calls = source.calls;
            bandlimited::take_trace();
            let turn = owner.step(&mut source, &mut out).unwrap();
            let trace = bandlimited::take_trace();
            assert!(
                trace.calls <= 1 && trace.input_samples <= 8192 && trace.output_samples <= 8192,
                "device pull ran multiple kernels in one turn"
            );
            match turn {
                Turn::Rendered(frames) => {
                    assert!(
                        frames <= 128 && source.calls == calls + 1 && trace.calls == 0,
                        "source render quantum exceeded one turn"
                    );
                    kinds[0] += 1;
                }
                Turn::Resampled => {
                    assert_eq!(source.calls, calls);
                    kinds[1] += trace.calls;
                }
                Turn::Published(frames) => {
                    assert!(
                        frames <= 128 && source.calls == calls && trace.calls == 0,
                        "FIFO publication exceeded its quantum"
                    );
                    kinds[2] += 1;
                }
                Turn::Finished => break,
                _ => panic!("unexpected pull turn"),
            }
            let b = owner.buffers.as_ref().unwrap();
            assert!(
                b.input.capacity() <= 128 * f.channels as usize
                    && owner.ready.output.capacity() <= 8192,
                "device pull scratch grew"
            );
            assert!(
                owner
                    .clocks()
                    .rendered_source
                    .saturating_sub(owner.clocks().published_source)
                    <= MAX_AHEAD,
                "device graph exceeded its lookahead bound"
            );
        }
        assert!(kinds.iter().all(|&n| n > 0));
        assert!(source.largest <= 128);
    }
}

#[test]
fn lookahead_and_event_budget_exhaustion_are_terminal_and_release_buffers() {
    let host = IoScheduler::new(0);
    for budget in [
        Limits {
            source_ahead: 1,
            pending_events: 64,
        },
        Limits {
            source_ahead: MAX_AHEAD,
            pending_events: 0,
        },
    ] {
        let (_route, mut owner) = pull(&host, fmt(44100, 48000, 1), budget);
        let mut source = Samples::new(audio(fmt(44100, 48000, 1), 4096));
        source.notice(1, 9);
        let mut output = [0.25; 128];
        let mut error = None;
        for _ in 0..1000 {
            if let Err(e) = owner.step(&mut source, &mut output) {
                error = Some(e.code);
                break;
            }
        }
        assert_eq!(
            error,
            Some(ErrorCode::InputSaturated),
            "device pull ignored a working-set budget"
        );
        assert!(
            owner.buffers.is_none()
                && owner.ready.stream.is_none()
                && owner.ready.output.capacity() == 0,
            "budget failure retained pull buffers"
        );
        assert!(output.iter().all(|&v| v == 0.25));
        let calls = source.calls;
        assert_eq!(
            owner.step(&mut source, &mut output).unwrap_err().code,
            ErrorCode::InputSaturated
        );
        assert_eq!(source.calls, calls);
    }
}

struct Graph {
    context: crate::context::AudioContext,
    frames: usize,
    total: usize,
    collected: Vec<u32>,
    inactive_with_work: bool,
}
impl Graph {
    fn new() -> Self {
        let mut context = crate::context::AudioContext::new(1, 96000, 1);
        for (id, n, value) in [(10, 1, 0.25), (11, 129, 0.5)] {
            let buffer = context
                .add_buffer(DecodedAudio {
                    samples: vec![value; n],
                    sample_rate: 96000,
                    channels: 1,
                })
                .unwrap();
            context.add_node(Box::new(crate::nodes::BufferSourceNode::new(id, 96000)));
            context.set_buffer(id, Some(buffer));
            context.connect(id, crate::context::DESTINATION_NODE_ID);
            context.watch_source_ended(id, true);
            assert!(context.start_source(id, 0.0, 0.0, None));
        }
        Self {
            context,
            frames: 0,
            total: 4096,
            collected: Vec::with_capacity(16),
            inactive_with_work: false,
        }
    }
}
impl Source for Graph {
    fn render(&mut self, out: &mut [f32]) -> EngineResult<Block> {
        let frames = out.len().min(self.total - self.frames);
        self.collected
            .extend_from_slice(self.context.process(&mut out[..frames]));
        self.frames += frames;
        if !self.context.has_active_sources() && self.frames < self.total {
            self.inactive_with_work = true;
        }
        Ok(Block {
            frames,
            eof: self.frames == self.total,
        })
    }
    fn drain_ended(&mut self) -> impl Iterator<Item = u32> {
        self.context.drain_ended()
    }
}

#[test]
fn graph_collection_is_immediate_but_ended_waits_for_device_fences() {
    let host = IoScheduler::new(0);
    let (_route, mut owner) = pull(&host, fmt(96000, 3000, 1), limits());
    let mut graph = Graph::new();
    let mut out = [0.0; 128];
    assert_eq!(
        owner.step(&mut graph, &mut out).unwrap(),
        Turn::Rendered(128)
    );
    assert_eq!(
        graph.collected,
        [10],
        "graph collection was delayed behind device PCM"
    );
    assert!(
        owner.pop_ended().unwrap().is_none(),
        "ended escaped during filter lookahead"
    );
    let (pcm, events) = collect(&mut owner, &mut graph, 128);
    assert_eq!(
        events,
        [(4, 10), (8, 11)],
        "ended did not stop at its exact device fence"
    );
    assert_eq!(graph.collected, [10, 11]);
    assert!(
        graph.inactive_with_work,
        "fixture did not render beyond source-node completion"
    );
    assert_eq!(
        graph.context.current_time(),
        owner.clocks().rendered_source as f64 / 96000.0
    );
    let mut reference_graph = Graph::new();
    let mut source_pcm = Vec::new();
    while reference_graph.frames < reference_graph.total {
        out.fill(0.0);
        reference_graph.render(&mut out).unwrap();
        source_pcm.extend_from_slice(&out);
    }
    let audio = DecodedAudio {
        samples: source_pcm,
        sample_rate: 96000,
        channels: 1,
    };
    assert_eq!(pcm, crate::resample_job::tests::reference(&audio, 3000));
}

#[test]
fn zero_duration_events_are_ordered_once_and_empty_output_does_no_work() {
    let host = IoScheduler::new(0);
    let (_route, mut owner) = pull(&host, fmt(44100, 48000, 2), limits());
    let mut source = Samples::new(audio(fmt(44100, 48000, 2), 0));
    for id in 0..16 {
        source.notice(0, id);
    }
    assert_eq!(owner.step(&mut source, &mut []).unwrap(), Turn::Idle);
    assert_eq!(source.calls, 0);
    let mut out = [0.25; 256];
    assert_eq!(
        owner.step(&mut source, &mut out).unwrap(),
        Turn::Rendered(0)
    );
    assert_eq!(
        owner.step(&mut source, &mut out).unwrap(),
        Turn::EventsReady
    );
    for id in 0..16 {
        assert_eq!(
            owner.pop_ended().unwrap(),
            Some(id),
            "zero-duration event reordered or repeated"
        );
    }
    assert_eq!(owner.pop_ended().unwrap(), None);
    let (pcm, events) = collect(&mut owner, &mut source, 3);
    assert!(pcm.is_empty() && events.is_empty());
    assert_eq!(owner.published, 0);
}

#[test]
fn suspend_and_partial_frame_calls_keep_fifo_phase_and_due_events() {
    let host = IoScheduler::new(0);
    let (_route, mut owner) = pull(&host, fmt(44100, 48000, 2), limits());
    let mut source = Samples::new(audio(fmt(44100, 48000, 2), 4096));
    source.notice(1, 7);
    let mut out = [0.0; 256];
    let mut prefix = Vec::new();
    for _ in 0..100000 {
        out.fill(0.0);
        let turn = owner.step(&mut source, &mut out).unwrap();
        if let Turn::Published(n) = turn {
            prefix.extend_from_slice(&out[..n * 2]);
        }
        if turn == Turn::EventsReady {
            break;
        }
    }
    let clock = owner.clocks();
    let calls = source.calls;
    let head = owner.output_head;
    owner.suspend();
    out.fill(0.25);
    for _ in 0..8 {
        assert_eq!(
            owner.step(&mut source, &mut out).unwrap(),
            Turn::Idle,
            "suspended device advanced a pull turn"
        );
    }
    assert_eq!(
        owner.pop_ended().unwrap(),
        None,
        "suspended device published an event"
    );
    assert_eq!(owner.clocks(), clock);
    assert_eq!(source.calls, calls);
    assert_eq!(owner.output_head, head);
    assert!(out.iter().all(|&v| v == 0.25));
    owner.resume();
    assert_eq!(owner.step(&mut source, &mut out[..1]).unwrap(), Turn::Idle);
    assert_eq!(owner.pop_ended().unwrap(), Some(7));
    let (rest, events) = collect(&mut owner, &mut source, 3);
    prefix.extend(rest);
    assert!(events.is_empty());
    assert_eq!(
        prefix,
        crate::resample_job::tests::reference(&audio(fmt(44100, 48000, 2), 4096), 48000),
        "suspend lost cached device PCM"
    );
}

#[test]
fn cancellation_revokes_fifo_and_events_before_cached_data_can_escape() {
    let host = IoScheduler::new(0);
    let (mut route, mut owner) = pull(&host, fmt(44100, 48000, 1), limits());
    let mut source = Samples::new(audio(fmt(44100, 48000, 1), 4096));
    source.notice(1, 7);
    fifo(&mut owner, &mut source);
    route.invalidate();
    let calls = source.calls;
    let mut out = [0.25; 128];
    assert_eq!(
        owner
            .step(&mut source, &mut out)
            .err()
            .map(|error| error.code),
        Some(ErrorCode::Cancelled),
        "cancelled FIFO published stale PCM"
    );
    assert!(out.iter().all(|&v| v == 0.25));
    assert_eq!(source.calls, calls);
    assert!(owner.buffers.is_none() && owner.ready.output.capacity() == 0);
    assert_eq!(owner.pop_ended().unwrap_err().code, ErrorCode::Cancelled);
}

#[test]
fn host_close_revokes_cached_pcm_and_already_due_events() {
    for event_first in [false, true] {
        let host = IoScheduler::new(0);
        let (_route, mut owner) = pull(&host, fmt(44100, 48000, 1), limits());
        let mut source = Samples::new(audio(
            fmt(44100, 48000, 1),
            if event_first { 0 } else { 4096 },
        ));
        source.notice(0, 7);
        let mut out = [0.25; 128];
        if event_first {
            owner.step(&mut source, &mut out).unwrap();
        } else {
            fifo(&mut owner, &mut source);
        }
        host.close();
        let expected = EngineError::from(migo_io::pools::PoolError::Closed).code;
        if event_first {
            assert_eq!(
                owner.pop_ended().err().map(|e| e.code),
                Some(expected),
                "cached event crossed host close"
            );
        } else {
            assert_eq!(
                owner.step(&mut source, &mut out).err().map(|e| e.code),
                Some(expected),
                "cached PCM crossed host close"
            );
        }
        assert!(owner.buffers.is_none() && owner.ready.output.capacity() == 0);
    }
}

#[test]
fn first_and_steady_pull_turns_allocate_no_rust_heap() {
    let host = IoScheduler::new(0);
    for f in [
        fmt(44100, 48000, 2),
        fmt(48000, 44100, 2),
        fmt(768000, 3000, 32),
        fmt(3000, 768000, 32),
    ] {
        let (_route, mut owner) = pull(&host, f, limits());
        let mut source = Samples::new(audio(f, 513));
        source.notice(1, 7);
        source.notice(129, 8);
        let mut out = vec![0.0; 128 * f.channels as usize];
        bandlimited::take_trace();
        let mut finished = false;
        for _ in 0..100000 {
            let before = migo_alloc_probe::thread_counts();
            let turn = owner.step(&mut source, &mut out).unwrap();
            while owner.pop_ended().unwrap().is_some() {}
            let after = migo_alloc_probe::thread_counts();
            assert_eq!(
                after.allocations, before.allocations,
                "device pull turn allocated"
            );
            assert_eq!(
                after.reallocations, before.reallocations,
                "device pull turn reallocated"
            );
            if turn == Turn::Finished {
                finished = true;
                break;
            }
        }
        assert!(finished);
    }
}

struct Bad(u8);
impl Source for Bad {
    fn render(&mut self, out: &mut [f32]) -> EngineResult<Block> {
        out.fill(0.9);
        match self.0 {
            0 => Ok(Block {
                frames: out.len() + 1,
                eof: false,
            }),
            1 => Ok(Block {
                frames: 0,
                eof: false,
            }),
            _ => Err(EngineError::from_detail(
                ErrorCode::NotFound,
                "source failure",
            )),
        }
    }
    fn drain_ended(&mut self) -> impl Iterator<Item = u32> {
        std::iter::empty()
    }
}
#[test]
fn invalid_source_blocks_and_errors_are_sticky_without_partial_success() {
    let host = IoScheduler::new(0);
    for mode in 0..3 {
        let (_route, mut owner) = pull(&host, fmt(44100, 48000, 1), limits());
        let mut source = Bad(mode);
        let mut out = [0.25; 128];
        let code = if mode < 2 {
            ErrorCode::InvalidArgument
        } else {
            ErrorCode::NotFound
        };
        assert_eq!(
            owner
                .step(&mut source, &mut out)
                .err()
                .map(|error| error.code),
            Some(code),
            "invalid source metadata was accepted"
        );
        assert_eq!(
            owner.step(&mut source, &mut out).unwrap_err().code,
            code,
            "device pull lost its terminal source error"
        );
        assert!(out.iter().all(|&v| v == 0.25));
        assert!(owner.buffers.is_none() && owner.ready.stream.is_none());
    }
}

#[test]
fn configuration_requires_a_live_prepared_converted_owner() {
    let host = IoScheduler::new(0);
    for budget in [
        Limits {
            source_ahead: 0,
            pending_events: 0,
        },
        Limits {
            source_ahead: MAX_AHEAD + 1,
            pending_events: 0,
        },
        Limits {
            source_ahead: MAX_AHEAD,
            pending_events: MAX_EVENTS + 1,
        },
    ] {
        let (_route, ready) = ready(&host, fmt(44100, 48000, 1));
        assert_eq!(
            crate::streaming::get_stream_runtime()
                .block_on(Pull::new(ready, budget))
                .err()
                .unwrap()
                .code,
            ErrorCode::InvalidArgument
        );
    }
    let (_route, bypass) = ready(&host, fmt(48000, 48000, 1));
    assert_eq!(
        crate::streaming::get_stream_runtime()
            .block_on(Pull::new(bypass, limits()))
            .err()
            .unwrap()
            .code,
        ErrorCode::InvalidArgument
    );
    let (mut route, ready) = ready(&host, fmt(44100, 48000, 1));
    route.invalidate();
    assert_eq!(
        crate::streaming::get_stream_runtime()
            .block_on(Pull::new(ready, limits()))
            .err()
            .unwrap()
            .code,
        ErrorCode::Cancelled
    );
}

#[test]
fn clock_overflow_is_terminal_before_output_is_modified() {
    let host = IoScheduler::new(0);
    let (_route, mut owner) = pull(&host, fmt(768000, 3000, 1), limits());
    owner.published = u64::MAX;
    assert_eq!(
        owner.clocks().published_source,
        u64::MAX,
        "device clock wrapped during rational scaling"
    );
    owner.rendered = u64::MAX - 64;
    let mut source = Samples::new(audio(fmt(768000, 3000, 1), 4096));
    let mut out = [0.25; 128];
    assert_eq!(
        owner.step(&mut source, &mut out).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert!(out.iter().all(|&v| v == 0.25));
    let (_route, mut owner) = pull(&host, fmt(44100, 48000, 1), limits());
    fifo(&mut owner, &mut source);
    owner.published = u64::MAX;
    out.fill(0.25);
    assert_eq!(
        owner.step(&mut source, &mut out).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert!(
        out.iter().all(|&v| v == 0.25),
        "overflow published partial device PCM"
    );
}
