use super::*;
use crate::{
    decoder::DecodedAudio,
    device_prepare::{Format, Slot},
};
use migo_io::scheduler::IoScheduler;
use shared::channel::ThreadWakeup;
use std::time::{Duration, Instant};

fn format(source: u32, target: u32, channels: u32) -> Format {
    Format {
        source,
        target,
        channels,
    }
}
fn session(host: &IoScheduler, f: Format) -> (Slot, Box<Session>) {
    let mut slot = Slot::new(1);
    slot.request(f, host.audio_cpu(), ThreadWakeup::new(), None)
        .unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    let ready = loop {
        if let Some(result) = slot.take() {
            break result.unwrap();
        }
        assert!(Instant::now() < end, "session preparation stalled");
        std::thread::yield_now();
    };
    let owner = crate::streaming::get_stream_runtime()
        .block_on(Session::new(
            ready,
            Limits {
                source_ahead: MAX_AHEAD,
                pending_events: 64,
            },
        ))
        .unwrap();
    (slot, owner)
}
struct Bursts {
    channels: usize,
    remaining: usize,
    control_active: bool,
    calls: usize,
    recorded: Vec<f32>,
    record: bool,
    emit: bool,
    ended: Option<u32>,
    eof: bool,
}
impl Bursts {
    fn new(channels: u32) -> Self {
        Self {
            channels: channels as usize,
            remaining: 0,
            control_active: false,
            calls: 0,
            recorded: Vec::new(),
            record: true,
            emit: false,
            ended: None,
            eof: false,
        }
    }
}
impl Graph for Bursts {
    fn active(&self) -> bool {
        self.remaining != 0 || self.control_active
    }
}
impl Source for Bursts {
    fn render(&mut self, output: &mut [f32]) -> EngineResult<Block> {
        self.calls += 1;
        let frames = output.len() / self.channels;
        let count = frames.min(self.remaining);
        for (i, sample) in output[..count * self.channels].iter_mut().enumerate() {
            *sample = ((self.remaining * 13 + i * 17) % 251) as f32 / 128.0 - 0.9;
        }
        self.remaining -= count;
        if self.emit && self.remaining == 0 {
            self.ended = Some(7);
            self.emit = false;
        }
        if self.record {
            self.recorded.extend_from_slice(output);
        }
        Ok(Block {
            frames,
            eof: self.eof,
        })
    }
    fn drain_ended(&mut self) -> impl Iterator<Item = u32> {
        self.ended.take().into_iter()
    }
}
fn drain(owner: &mut Session, graph: &mut Bursts, partition: usize) -> Vec<f32> {
    let mut output = vec![0.0; partition * graph.channels];
    let mut pcm = Vec::new();
    let start = owner.pull.published;
    for _ in 0..300000 {
        output.fill(0.0);
        let turn = owner.step(graph, &mut output).unwrap();
        if let SessionTurn::Work(Turn::Published(n)) = turn {
            pcm.extend_from_slice(&output[..n * graph.channels]);
        }
        while owner.pop_ended().unwrap().is_some() {}
        if turn == SessionTurn::Quiescent {
            assert_eq!(
                pcm.len() as u64,
                (owner.pull.published - start) * graph.channels as u64,
                "last publication was replaced by an idle notification"
            );
            return pcm;
        }
    }
    panic!("continuous graph never became quiescent");
}
fn identity(owner: &Session) -> (usize, usize, usize) {
    (
        &*owner.pull.ready as *const Ready as usize,
        owner.pull.ready.output.as_ptr() as usize,
        owner.pull.buffers.as_ref().unwrap().input.as_ptr() as usize,
    )
}

#[test]
fn inactive_startup_does_no_source_or_filter_work() {
    let host = IoScheduler::new(0);
    let (_slot, mut owner) = session(&host, format(44100, 48000, 2));
    let mut graph = Bursts::new(2);
    let before = owner.pull.clocks();
    let mut out = [0.25; 256];
    bandlimited::take_trace();
    for _ in 0..1000 {
        assert_eq!(
            owner.step(&mut graph, &mut out).unwrap(),
            SessionTurn::Quiescent,
            "inactive startup ran the continuous graph"
        );
    }
    assert_eq!(graph.calls, 0, "inactive startup rendered source frames");
    assert_eq!(bandlimited::take_trace().calls, 0);
    assert_eq!(owner.pull.clocks(), before);
    assert_eq!(out, [0.25; 256]);
}

#[test]
fn multiple_bursts_preserve_mature_pcm_phase_and_complete_tail() {
    let host = IoScheduler::new(0);
    for f in [
        format(44100, 48000, 1),
        format(48000, 44100, 2),
        format(96000, 8000, 2),
        format(768000, 3000, 2),
        format(3000, 768000, 2),
        format(44100, 48000, 32),
    ] {
        let (_slot, mut owner) = session(&host, f);
        let before = identity(&owner);
        let mut graph = Bursts::new(f.channels);
        let mut pcm = Vec::new();
        for (frames, partition) in [(257, 127), (513, 3), (129, 128)] {
            graph.remaining = frames;
            pcm.extend(drain(&mut owner, &mut graph, partition));
            let mut input = graph.recorded.clone();
            input.resize(input.len() + 4096 * f.channels as usize, 0.0);
            let reference = crate::resample_job::tests::reference(
                &DecodedAudio {
                    sample_rate: f.source,
                    channels: f.channels,
                    samples: input,
                },
                f.target,
            );
            assert!(pcm.len() <= reference.len());
            assert_eq!(
                pcm,
                reference[..pcm.len()],
                "idle restart changed mature PCM or sample phase"
            );
            assert!(
                reference[pcm.len()..].iter().all(|&x| x == 0.0),
                "quiescence discarded a nonzero mature filter tail"
            );
            assert_eq!(
                identity(&owner),
                before,
                "idle wake replaced the prepared owner or scratch"
            );
            assert!(
                !owner.pull.source_eof && !owner.pull.filter_done,
                "idle submitted EOF to the persistent filter"
            );
            assert!(owner.pull.clocks().published_source >= owner.quiet_at.unwrap());
        }
    }
}

#[test]
fn first_active_tail_idle_and_wake_turns_are_bounded_and_allocate_nothing() {
    let host = IoScheduler::new(0);
    let (_slot, mut owner) = session(&host, format(48000, 44100, 2));
    let mut graph = Bursts::new(2);
    graph.record = false;
    let mut out = [0.0; 256];
    for _ in 0..2 {
        graph.remaining = 257;
        let mut idle = 0;
        for _ in 0..100000 {
            let calls = graph.calls;
            bandlimited::take_trace();
            let before = migo_alloc_probe::thread_counts();
            let turn = owner.step(&mut graph, &mut out).unwrap();
            let after = migo_alloc_probe::thread_counts();
            assert_eq!(
                after.allocations, before.allocations,
                "session turn allocated"
            );
            assert_eq!(
                after.reallocations, before.reallocations,
                "session turn reallocated"
            );
            let trace = bandlimited::take_trace();
            assert!(
                trace.calls <= 1 && trace.input_samples <= 8192 && trace.output_samples <= 8192,
                "session exceeded one bounded mature kernel"
            );
            if turn == SessionTurn::Quiescent {
                assert_eq!(graph.calls, calls, "idle session rendered source frames");
                assert_eq!(trace.calls, 0, "idle session ran a mature kernel");
                idle += 1;
                if idle == 1000 {
                    break;
                }
            }
        }
        assert_eq!(idle, 1000, "idle session never stopped bounded work");
    }
}

#[test]
fn reactivation_clears_the_old_quiet_deadline_and_control_work_stays_active() {
    let host = IoScheduler::new(0);
    let (_slot, mut owner) = session(&host, format(44100, 48000, 1));
    let mut graph = Bursts::new(1);
    let mut out = [0.0; 128];
    graph.remaining = 1;
    while owner.quiet_at.is_none() {
        owner.step(&mut graph, &mut out).unwrap();
    }
    let old = owner.quiet_at.unwrap();
    graph.remaining = 1024;
    owner.step(&mut graph, &mut out).unwrap();
    assert_eq!(
        owner.quiet_at, None,
        "reactivation retained the old quiet deadline"
    );
    graph.control_active = true;
    for _ in 0..100 {
        assert_ne!(
            owner.step(&mut graph, &mut out).unwrap(),
            SessionTurn::Quiescent
        );
    }
    assert_eq!(
        owner.quiet_at, None,
        "scheduled or effect control work was treated as idle"
    );
    graph.control_active = false;
    drain(&mut owner, &mut graph, 128);
    assert!(
        owner.quiet_at.unwrap() > old,
        "new activity reused the previous tail fence"
    );
}

#[test]
fn unread_ended_fences_prevent_quiescence_and_are_delivered_once() {
    let host = IoScheduler::new(0);
    let (_slot, mut owner) = session(&host, format(44100, 48000, 1));
    let mut graph = Bursts::new(1);
    graph.remaining = 1;
    graph.emit = true;
    let mut out = [0.0; 128];
    let mut reached = false;
    for _ in 0..10000 {
        let turn = owner.step(&mut graph, &mut out).unwrap();
        assert_ne!(
            turn,
            SessionTurn::Quiescent,
            "idle hid an unread ended event"
        );
        if turn == SessionTurn::Work(Turn::EventsReady) {
            reached = true;
            break;
        }
    }
    assert!(reached);
    assert_eq!(owner.pull.published, (128_u64 * 48000).div_ceil(44100));
    for _ in 0..3 {
        assert_eq!(
            owner.step(&mut graph, &mut out).unwrap(),
            SessionTurn::Work(Turn::EventsReady)
        );
    }
    assert_eq!(owner.pop_ended().unwrap(), Some(7));
    assert_eq!(owner.pop_ended().unwrap(), None);
    drain(&mut owner, &mut graph, 128);
    assert!(owner.sleeping);
    // A reached tail fence still cannot hide a subsequently pending event.
    owner.sleeping = false;
    owner.quiet_at = Some(0);
    owner
        .pull
        .buffers
        .as_mut()
        .unwrap()
        .events
        .push_back(Event { id: 8, fence: 0 });
    assert_eq!(
        owner.step(&mut graph, &mut out).unwrap(),
        SessionTurn::Work(Turn::EventsReady),
        "idle hid an unread ended event"
    );
    assert!(!owner.sleeping, "idle hid an unread ended event");
}

#[test]
fn suspension_empty_and_partial_destinations_freeze_tail_and_clocks() {
    let host = IoScheduler::new(0);
    let (_slot, mut owner) = session(&host, format(44100, 48000, 2));
    let mut graph = Bursts::new(2);
    graph.remaining = 1;
    let mut out = [0.25; 256];
    while owner.quiet_at.is_none() {
        owner.step(&mut graph, &mut out).unwrap();
    }
    let clocks = owner.pull.clocks();
    let fence = owner.quiet_at;
    let calls = graph.calls;
    owner.suspend();
    for _ in 0..5 {
        assert_eq!(
            owner.step(&mut graph, &mut out).unwrap(),
            SessionTurn::Work(Turn::Idle)
        );
    }
    owner.resume();
    for n in [0, 1] {
        assert_eq!(
            owner.step(&mut graph, &mut out[..n]).unwrap(),
            SessionTurn::Work(Turn::Idle)
        );
    }
    assert_eq!(
        owner.pull.clocks(),
        clocks,
        "paused or partial callback advanced the session clock"
    );
    assert_eq!(
        owner.quiet_at, fence,
        "paused callback changed the tail fence"
    );
    assert_eq!(graph.calls, calls);
    drain(&mut owner, &mut graph, 128);
}

#[test]
fn cancellation_and_host_close_revoke_even_a_sleeping_owner() {
    for close in [false, true] {
        let host = IoScheduler::new(0);
        let (_slot, mut owner) = session(&host, format(44100, 48000, 1));
        let mut graph = Bursts::new(1);
        let mut out = [0.25; 128];
        let expected = if close {
            host.close();
            EngineError::from(migo_io::pools::PoolError::Closed).code
        } else {
            owner
                .pull
                .ready
                .request
                .cancel
                .store(true, Ordering::Release);
            ErrorCode::Cancelled
        };
        for _ in 0..2 {
            assert_eq!(
                owner.step(&mut graph, &mut out).err().map(|e| e.code),
                Some(expected),
                "sleeping session bypassed owner revocation"
            );
            assert_eq!(owner.pop_ended().err().map(|e| e.code), Some(expected));
        }
        assert!(owner.pull.buffers.is_none() && owner.pull.ready.output.capacity() == 0);
        assert_eq!(out, [0.25; 128]);
    }
}

#[test]
fn continuous_graph_eof_is_a_sticky_terminal_error() {
    let host = IoScheduler::new(0);
    let (_slot, mut owner) = session(&host, format(44100, 48000, 1));
    let mut graph = Bursts::new(1);
    graph.remaining = 1;
    graph.eof = true;
    let mut out = [0.0; 128];
    for _ in 0..2 {
        assert_eq!(
            owner.step(&mut graph, &mut out).err().map(|e| e.code),
            Some(ErrorCode::InvalidArgument),
            "continuous graph EOF was accepted as an idle flush"
        );
    }
    assert!(owner.pull.buffers.is_none());
}

#[test]
fn quiet_bound_uses_prepared_mature_history_and_all_cascade_rates() {
    use rubato::Resampler;
    let host = IoScheduler::new(0);
    for f in [
        format(44100, 48000, 2),
        format(768000, 3000, 32),
        format(3000, 768000, 32),
    ] {
        let (_slot, owner) = session(&host, f);
        let points = rubato::SincInterpolationParameters::default()
            .sinc_len
            .next_multiple_of(8);
        let mut rate = f.source;
        let mut expected = 0_u128;
        for next in crate::streaming_sinc::rates(f.source, f.target) {
            let kernel = crate::streaming_sinc::kernel(rate, next, f.channels).unwrap();
            expected += ((2 * points + kernel.input_frames_max() + 4) as u128 * f.source as u128)
                .div_ceil(rate as u128);
            rate = next;
        }
        assert_eq!(
            owner.quiet_span as u128, expected,
            "quiet bound lost a mature history window or source-rate projection"
        );
    }
    let mut raw = crate::streaming_sinc::Stream::new(44100, 48000, 1, None).unwrap();
    assert!(
        raw.quiet_source_frames().is_err(),
        "unprepared SRC exposed a quiet bound"
    );
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let mut out = Vec::with_capacity(8192);
    while raw.is_preparing() {
        raw.step(&[], false, &mut out, &cancel).unwrap();
    }
    raw.step(&[], true, &mut out, &cancel).unwrap();
    assert!(
        raw.quiet_source_frames().is_err(),
        "EOF SRC exposed a reusable quiet bound"
    );
}

#[test]
fn quiet_clock_overflow_and_cached_nonzero_pcm_cannot_disappear() {
    let host = IoScheduler::new(0);
    let (_slot, mut owner) = session(&host, format(44100, 48000, 1));
    let mut graph = Bursts::new(1);
    let mut out = [0.0; 128];
    owner.sleeping = false;
    owner.quiet_at = Some(0);
    owner.pull.ready.output.extend_from_slice(&[0.75; 256]);
    assert_eq!(
        owner.step(&mut graph, &mut out).unwrap(),
        SessionTurn::Work(Turn::Published(128))
    );
    assert!(!owner.sleeping, "quiescence discarded cached nonzero PCM");
    assert_eq!(out, [0.75; 128]);
    assert_eq!(
        owner.step(&mut graph, &mut out).unwrap(),
        SessionTurn::Work(Turn::Published(128)),
        "last publication was replaced by an idle notification"
    );
    let (_slot2, mut overflow) = session(&host, format(44100, 48000, 1));
    overflow.sleeping = false;
    overflow.pull.rendered = u64::MAX;
    overflow.pull.ready.output.extend_from_slice(&[0.0; 128]);
    assert_eq!(
        overflow.step(&mut graph, &mut out).err().map(|e| e.code),
        Some(ErrorCode::InvalidArgument),
        "quiet source fence wrapped instead of retiring"
    );
    assert!(overflow.pull.buffers.is_none());
}
