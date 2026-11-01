use super::*;
use crate::decoder::DecodedAudio;
use migo_io::scheduler::IoScheduler;
use std::{
    sync::{Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

fn format(source: u32, target: u32, channels: u32) -> Format {
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
fn host(
    cpu: AudioCpu,
    owners: usize,
    process: Arc<Usage>,
    aggregate: Arc<AudioAggregateLedger>,
) -> Host {
    Host {
        cpu: cpu.background(),
        usage: Usage::new(owners),
        process,
        aggregate,
        retire_hook: None,
    }
}
fn local(scheduler: &IoScheduler) -> Host {
    host(
        scheduler.audio_cpu(),
        2,
        Usage::new(4),
        Arc::new(AudioAggregateLedger::new(4 * OWNER_BYTES)),
    )
}
fn device(host: &Host, f: Format) -> Device {
    crate::streaming::get_stream_runtime()
        .block_on(host.prepare(1, f, limits(), None))
        .unwrap()
}
fn until(mut done: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < end, "managed owner failed to retire");
        thread::yield_now();
    }
}
fn empty(host: &Host) {
    until(|| {
        host.usage.owners.load(Ordering::Acquire) == 0
            && host.process.owners.load(Ordering::Acquire) == 0
            && host.aggregate.used_bytes() == 0
    });
}
struct Release(Option<mpsc::Sender<()>>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.take();
    }
}
fn held_retirement(host: &mut Host) -> (mpsc::Receiver<()>, Release) {
    let (tx, rx) = mpsc::channel();
    let (unlock, wait) = mpsc::channel::<()>();
    let wait = Mutex::new(Some(wait));
    host.retire_hook = Some(Arc::new(move |before| {
        if before && let Some(wait) = wait.lock().unwrap().take() {
            tx.send(()).unwrap();
            let _ = wait.recv_timeout(Duration::from_secs(10));
        }
    }));
    (rx, Release(Some(unlock)))
}

#[test]
fn closed_host_retirement_keeps_the_streaming_worker_available() {
    let scheduler = IoScheduler::new(0);
    let mut host = local(&scheduler);
    let (entered, release) = held_retirement(&mut host);
    let owner = device(&host, format(44_100, 48_000, 2));
    scheduler.close();
    drop(owner);
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    let (sent, pulse) = mpsc::channel();
    crate::streaming::get_stream_runtime().spawn(async move {
        let _ = sent.send(());
    });
    let responsive = pulse.recv_timeout(Duration::from_millis(200)).is_ok();
    let physical = host.aggregate.used_bytes();
    // Always unblock the destructor before asserting, including the old path.
    drop(release);
    empty(&host);
    assert_eq!(
        physical, OWNER_BYTES,
        "retirement released physical credit early"
    );
    assert!(
        responsive,
        "closed-host destruction occupied the shared streaming worker"
    );
}

#[test]
fn a_closed_four_host_burst_destroys_all_32_owners_on_cpu_workers() {
    super::preparation_tests::concurrent_burst();
    let process = Usage::new(PROCESS_OWNERS);
    let aggregate = Arc::new(AudioAggregateLedger::new(PROCESS_OWNERS * OWNER_BYTES));
    let primary = IoScheduler::isolated_for_admission_probe(0, 6, 128 * 1024 * 1024);
    let live = primary.peer_for_admission_probe(4);
    let mut schedulers: Vec<_> = (1..4)
        .map(|id| primary.peer_for_admission_probe(id))
        .collect();
    schedulers.push(primary);
    let start = Arc::new(Mutex::new(None::<Instant>));
    let events = Arc::new(Mutex::new(Vec::with_capacity(64)));
    let (sent, entered) = mpsc::channel();
    let (unlock, wait) = mpsc::channel::<()>();
    let wait = Mutex::new(Some(wait));
    let hook: RetireHook = Arc::new({
        let start = start.clone();
        let events = events.clone();
        move |before| {
            let elapsed = start.lock().unwrap().unwrap().elapsed();
            events.lock().unwrap().push((
                before,
                thread::current().name().unwrap_or("unnamed").to_owned(),
                elapsed,
            ));
            if before && let Some(wait) = wait.lock().unwrap().take() {
                let _ = sent.send(());
                let _ = wait.recv_timeout(Duration::from_secs(10));
            }
        }
    });
    let hosts: Vec<_> = schedulers
        .iter()
        .map(|scheduler| {
            let mut host = host(
                scheduler.audio_cpu(),
                HOST_OWNERS,
                process.clone(),
                aggregate.clone(),
            );
            host.retire_hook = Some(hook.clone());
            host
        })
        .collect();
    let formats = [
        format(44_100, 48_000, 2),
        format(48_000, 44_100, 2),
        format(3_000, 768_000, 32),
        format(768_000, 3_000, 32),
    ];
    let mut owners = Vec::with_capacity(PROCESS_OWNERS);
    for host in &hosts {
        for n in 0..HOST_OWNERS {
            owners.push(device(host, formats[n % formats.len()]));
        }
    }
    const MP3: &[u8] = include_bytes!("decoder/mp3/fixtures/stereo-cbr.mp3");
    let reference = crate::decoder::decode(MP3).unwrap();
    assert!(reference.samples.iter().any(|&value| value != 0.0));
    assert_eq!(aggregate.used_bytes(), PROCESS_OWNERS * OWNER_BYTES);
    for scheduler in &schedulers {
        scheduler.close();
    }
    *start.lock().unwrap() = Some(Instant::now());
    let release = Release(Some(unlock));
    drop(owners);
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    let (sent, pulse) = mpsc::channel();
    crate::streaming::get_stream_runtime().spawn(async move {
        let _ = sent.send(());
    });
    let responsive = pulse.recv_timeout(Duration::from_millis(200)).is_ok();
    let (codec_sent, codec_reply) = mpsc::channel();
    let cpu = live.audio_cpu();
    crate::streaming::get_stream_runtime().spawn(async move {
        let result = cpu
            .run(|| crate::decoder::decode(MP3))
            .await
            .and_then(|result| result);
        let _ = codec_sent.send(result);
    });
    let (io_sent, io_reply) = mpsc::channel();
    let _io_reply = live
        .pools()
        .submit_async(
            migo_io::task::PoolKind::Fs,
            migo_io::task::PriorityClass::ForegroundAsync,
            move || {
                let _ = io_sent.send(std::fs::read(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/src/decoder/mp3/fixtures/stereo-cbr.mp3"
                )));
            },
        )
        .unwrap();
    let decoded = codec_reply.recv_timeout(Duration::from_secs(2));
    let decoded_while_held = matches!(&decoded, Ok(Ok(_)));
    let read = io_reply.recv_timeout(Duration::from_secs(2));
    let io_while_held = matches!(&read, Ok(Ok(_)));
    let outstanding = aggregate.used_bytes();
    let released_at = start.lock().unwrap().unwrap().elapsed();
    drop(release);
    let decoded =
        decoded.unwrap_or_else(|_| codec_reply.recv_timeout(Duration::from_secs(10)).unwrap());
    let read = read.unwrap_or_else(|_| io_reply.recv_timeout(Duration::from_secs(10)).unwrap());
    for host in &hosts {
        empty(host);
    }
    until(|| {
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|(before, _, _)| !before)
            .count()
            == PROCESS_OWNERS
    });
    let events = events.lock().unwrap();
    assert!(
        responsive,
        "closed-host burst occupied the shared streaming worker"
    );
    assert!(
        decoded_while_held,
        "retirement burst blocked live codec CPU work"
    );
    assert!(
        io_while_held,
        "retirement burst blocked live filesystem work"
    );
    let decoded = decoded.unwrap();
    assert_eq!(
        (decoded.sample_rate, decoded.channels),
        (reference.sample_rate, reference.channels)
    );
    assert!(
        decoded
            .samples
            .iter()
            .map(|v| v.to_bits())
            .eq(reference.samples.iter().map(|v| v.to_bits())),
        "live codec PCM changed during retirement"
    );
    assert_eq!(
        read.unwrap(),
        MP3,
        "live filesystem bytes changed during retirement"
    );
    assert!(
        outstanding >= OWNER_BYTES,
        "burst released the blocked owner's physical credit"
    );
    assert_eq!(
        events.len(),
        PROCESS_OWNERS * 2,
        "burst did not destroy each owner exactly once"
    );
    assert!(
        events
            .iter()
            .all(|(_, name, _)| name.starts_with("Migo-IO-")),
        "burst destroyed an owner outside the existing CPU executor"
    );
    let finished = events
        .iter()
        .filter(|(before, _, _)| !before)
        .map(|(_, _, time)| *time)
        .max()
        .unwrap();
    eprintln!(
        "closed four-host retirement: owners=32 reserved_bytes={} complete_us={} held_us={} after_release_us={}",
        PROCESS_OWNERS * OWNER_BYTES,
        finished.as_micros(),
        released_at.as_micros(),
        finished.saturating_sub(released_at).as_micros()
    );
}
struct Pulse {
    remaining: usize,
    channels: usize,
    recorded: Vec<f32>,
    bad: bool,
}
impl Graph for Pulse {
    fn active(&self) -> bool {
        self.remaining > 0 || self.bad
    }
}
impl Source for Pulse {
    fn render(&mut self, out: &mut [f32]) -> EngineResult<Block> {
        if self.bad {
            return Err(EngineError::from_detail(
                ErrorCode::NotFound,
                "graph source failed",
            ));
        }
        let frames = out.len() / self.channels;
        let n = frames.min(self.remaining);
        for (i, x) in out[..n * self.channels].iter_mut().enumerate() {
            *x = ((i + self.remaining * 17) % 251) as f32 / 128.0 - 0.9;
        }
        self.remaining -= n;
        self.recorded.extend_from_slice(out);
        Ok(Block { frames, eof: false })
    }
    fn drain_ended(&mut self) -> impl Iterator<Item = u32> {
        std::iter::empty()
    }
}
fn pulse(channels: usize) -> Pulse {
    Pulse {
        remaining: 0,
        channels,
        recorded: Vec::new(),
        bad: false,
    }
}

#[test]
fn default_process_and_cloned_host_use_the_same_resource_authorities() {
    let a = IoScheduler::new(0);
    let b = IoScheduler::new(0);
    let first = Host::new(a.audio_cpu());
    let clone = first.clone();
    let second = Host::new(b.audio_cpu());
    assert!(
        Arc::ptr_eq(&first.usage, &clone.usage),
        "host clone duplicated its resource quota"
    );
    assert!(
        Arc::ptr_eq(&first.process, &second.process),
        "hosts do not share the process filter quota"
    );
    assert!(
        Arc::ptr_eq(&first.aggregate, &second.aggregate)
            && Arc::ptr_eq(&first.aggregate, &AudioAggregateLedger::process_global()),
        "filters bypass the physical PCM ledger"
    );
    assert!(!Arc::ptr_eq(&first.usage, &second.usage));
}
#[test]
fn exact_host_process_limits_and_rejection_rollback_hold() {
    let scheduler = IoScheduler::new(0);
    let process = Usage::new(3);
    let aggregate = Arc::new(AudioAggregateLedger::new(4 * OWNER_BYTES));
    let a = host(scheduler.audio_cpu(), 2, process.clone(), aggregate.clone());
    let b = host(scheduler.audio_cpu(), 2, process, aggregate);
    let f = format(44100, 48000, 2);
    let one = a.reserve(f).unwrap();
    let two = a.clone().reserve(f).unwrap();
    assert_eq!(
        a.reserve(f).err().map(|e| e.code),
        Some(ErrorCode::InputSaturated),
        "host accepted an owner beyond its exact quota"
    );
    let three = b.reserve(f).unwrap();
    assert_eq!(
        b.reserve(f).err().map(|e| e.code),
        Some(ErrorCode::InputSaturated),
        "process accepted an owner beyond its exact quota"
    );
    assert_eq!(
        b.usage.owners.load(Ordering::Acquire),
        1,
        "failed process admission leaked host credit"
    );
    assert_eq!(a.aggregate.used_bytes(), 3 * OWNER_BYTES);
    drop(two);
    let replacement = b.reserve(f).unwrap();
    drop((one, three, replacement));
    empty(&a);
    empty(&b);
}
#[test]
fn aggregate_pcm_competition_rejects_before_any_filter_construction() {
    let scheduler = IoScheduler::new(0);
    let h = host(
        scheduler.audio_cpu(),
        2,
        Usage::new(3),
        Arc::new(AudioAggregateLedger::new(OWNER_BYTES)),
    );
    let pcm = h.aggregate.try_reserve(1, "existing PCM").unwrap();
    let called = Arc::new(AtomicUsize::new(0));
    let seen = called.clone();
    let hook: Hook = Arc::new(move |_| {
        seen.fetch_add(1, Ordering::AcqRel);
    });
    let result = crate::streaming::get_stream_runtime().block_on(h.prepare(
        1,
        format(44100, 48000, 2),
        limits(),
        Some(hook),
    ));
    assert_eq!(
        result.err().map(|e| e.code),
        Some(ErrorCode::InputSaturated),
        "filter construction bypassed physical admission"
    );
    assert_eq!(
        called.load(Ordering::Acquire),
        0,
        "filter was constructed before its memory reservation"
    );
    assert_eq!(h.usage.owners.load(Ordering::Acquire), 0);
    assert_eq!(h.process.owners.load(Ordering::Acquire), 0);
    assert_eq!(h.aggregate.used_bytes(), 1);
    drop(pcm);
    drop(device(&h, format(44100, 48000, 2)));
    empty(&h);
}
#[test]
fn aborted_running_preparation_holds_credit_until_its_actual_owner_dies() {
    super::preparation_tests::permit_lifetime();
    let scheduler = IoScheduler::new(0);
    let h = local(&scheduler);
    let (tx, started) = mpsc::channel();
    let (unlock, wait) = mpsc::channel::<()>();
    let wait = Mutex::new(Some(wait));
    let hook: Hook = Arc::new(move |r| {
        if r.stream.as_ref().unwrap().prepared_layers() == 1
            && let Some(wait) = wait.lock().unwrap().take()
        {
            tx.send(()).unwrap();
            let _ = wait.recv_timeout(Duration::from_secs(10));
        }
    });
    let cloned = h.clone();
    let task = crate::streaming::get_stream_runtime().spawn(async move {
        cloned
            .prepare(1, format(768000, 3000, 2), limits(), Some(hook))
            .await
    });
    started.recv_timeout(Duration::from_secs(10)).unwrap();
    crate::streaming::get_stream_runtime().block_on(async {
        task.abort();
        assert!(task.await.err().is_some_and(|error| error.is_cancelled()));
    });
    assert_eq!(
        h.aggregate.used_bytes(),
        OWNER_BYTES,
        "aborting a running preparation returned credit before the CPU owner died"
    );
    assert_eq!(h.usage.owners.load(Ordering::Acquire), 1);
    drop(unlock);
    empty(&h);
}
#[test]
fn reserved_retirement_handoff_is_bounded_and_keeps_credit_during_backlog() {
    let scheduler = IoScheduler::new(0);
    let mut h = host(
        scheduler.audio_cpu(),
        1,
        Usage::new(1),
        Arc::new(AudioAggregateLedger::new(OWNER_BYTES)),
    );
    let (started, release) = held_retirement(&mut h);
    let mut d = device(&h, format(768000, 3000, 32));
    let before = migo_alloc_probe::thread_counts();
    d.retire(ErrorCode::Cancelled);
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        before.allocations, after.allocations,
        "retirement handoff allocated"
    );
    assert_eq!(
        before.reallocations, after.reallocations,
        "retirement handoff reallocated"
    );
    assert!(
        after.bytes_freed - before.bytes_freed < 1024,
        "render caller destroyed a heavy owner during handoff"
    );
    started.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(
        h.aggregate.used_bytes(),
        OWNER_BYTES,
        "retirement backlog returned physical credit before destruction"
    );
    assert_eq!(
        h.reserve(format(44100, 48000, 2)).err().map(|e| e.code),
        Some(ErrorCode::InputSaturated),
        "retired owner stopped counting against the owner cap"
    );
    drop(release);
    empty(&h);
    assert!(h.reserve(format(44100, 48000, 2)).is_ok());
}
#[test]
fn normal_drop_and_closed_host_destroy_resources_off_the_calling_thread() {
    for close in [false, true] {
        let scheduler = IoScheduler::new(0);
        let mut h = local(&scheduler);
        let (tx, rx) = mpsc::channel();
        let original = thread::current().id();
        let ledger = h.aggregate.clone();
        h.retire_hook = Some(Arc::new(move |before| {
            tx.send((before, thread::current().id(), ledger.used_bytes()))
                .unwrap();
        }));
        let mut d = device(&h, format(44100, 48000, 2));
        if close {
            scheduler.close();
            let mut graph = pulse(2);
            let mut output = [0.25; 256];
            assert_eq!(
                d.step(&mut graph, &mut output).err().map(|e| e.code),
                Some(EngineError::from(migo_io::pools::PoolError::Closed).code),
                "closed managed owner lost its typed error"
            );
            assert_eq!(output, [0.25; 256]);
        }
        drop(d);
        let (before, thread, bytes) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(before);
        assert_ne!(
            thread, original,
            "heavy owner destruction ran on the caller"
        );
        assert_eq!(
            bytes, OWNER_BYTES,
            "credit was released before actual destruction started"
        );
        let (before, thread, bytes) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(!before);
        assert_ne!(thread, original);
        assert_eq!(bytes, 0, "physical credit survived completed destruction");
        empty(&h);
    }
}
#[test]
fn terminal_source_errors_retain_cached_filters_until_the_reserved_handoff() {
    let scheduler = IoScheduler::new(0);
    let mut h = local(&scheduler);
    let (started, release) = held_retirement(&mut h);
    let mut d = device(&h, format(44100, 48000, 2));
    let mut graph = pulse(2);
    graph.bad = true;
    let mut out = [0.25; 256];
    let before = migo_alloc_probe::thread_counts();
    let result = d.step(&mut graph, &mut out);
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(
        result.err().map(|e| e.code),
        Some(ErrorCode::NotFound),
        "managed source error lost its typed code"
    );
    assert!(
        after.bytes_freed - before.bytes_freed < 1024,
        "source error destroyed a filter or scratch on the render caller"
    );
    started.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(h.aggregate.used_bytes(), OWNER_BYTES);
    assert!(d.lease.is_none());
    for _ in 0..3 {
        assert_eq!(
            d.step(&mut graph, &mut out).err().map(|e| e.code),
            Some(ErrorCode::NotFound)
        );
        assert_eq!(
            d.pop_ended().err().map(|e| e.code),
            Some(ErrorCode::NotFound)
        );
    }
    assert_eq!(out, [0.25; 256]);
    drop(release);
    empty(&h);
}
#[test]
fn internal_src_cancel_race_retains_tables_for_off_render_retirement() {
    let scheduler = IoScheduler::new(0);
    let mut h = local(&scheduler);
    let (started, release) = held_retirement(&mut h);
    let mut d = device(&h, format(768000, 3000, 32));
    let ready = &mut d.session().unwrap().pull.ready;
    let layers = ready.stream.as_ref().unwrap().prepared_layers();
    ready.request.cancel.store(true, Ordering::Release);
    let before = migo_alloc_probe::thread_counts();
    let result =
        ready
            .stream
            .as_mut()
            .unwrap()
            .step(&[], false, &mut ready.output, &ready.request.cancel);
    let after = migo_alloc_probe::thread_counts();
    assert_eq!(result.err().map(|e| e.code), Some(ErrorCode::Cancelled));
    assert_eq!(
        ready.stream.as_ref().unwrap().prepared_layers(),
        layers,
        "internal SRC cancellation destroyed live filter tables"
    );
    assert!(
        after.bytes_freed - before.bytes_freed < 1024,
        "internal SRC error deallocated filters on its caller"
    );
    d.retire(ErrorCode::Cancelled);
    started.recv_timeout(Duration::from_secs(10)).unwrap();
    drop(release);
    empty(&h);
}
#[test]
fn managed_lifetime_preserves_complete_mature_pcm_across_idle_and_restart() {
    let scheduler = IoScheduler::new(0);
    let h = local(&scheduler);
    for f in [
        format(44100, 48000, 1),
        format(48000, 44100, 2),
        format(768000, 3000, 2),
        format(3000, 768000, 2),
    ] {
        let mut d = device(&h, f);
        let mut graph = pulse(f.channels as usize);
        let mut pcm = Vec::new();
        let mut out = vec![0.0; 127 * f.channels as usize];
        for frames in [257, 129] {
            graph.remaining = frames;
            let mut done = false;
            for _ in 0..300000 {
                out.fill(0.0);
                let turn = d.step(&mut graph, &mut out).unwrap();
                if let SessionTurn::Work(Turn::Published(n)) = turn {
                    pcm.extend_from_slice(&out[..n * f.channels as usize]);
                }
                if turn == SessionTurn::Quiescent {
                    done = true;
                    break;
                }
            }
            assert!(done);
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
            assert_eq!(
                pcm,
                reference[..pcm.len()],
                "managed retirement/admission changed mature PCM or restart phase"
            );
            assert!(reference[pcm.len()..].iter().all(|&x| x == 0.0));
            assert_eq!(
                h.aggregate.used_bytes(),
                OWNER_BYTES,
                "active or idle session lost its reservation"
            );
        }
        drop(d);
        empty(&h);
    }
}
#[test]
fn cumulative_native_construction_allocations_fit_the_reserved_peak() {
    // All Rust allocations over a construction exceed its simultaneous peak.
    // Dedicated threads exclude concurrent tests and unrelated CPU task reuse.
    let scheduler = IoScheduler::new(0);
    let cpu = scheduler.audio_cpu();
    for f in [
        format(44100, 48000, 1),
        format(48000, 44100, 2),
        format(768000, 3000, 32),
        format(3000, 768000, 32),
    ] {
        let cpu = cpu.clone();
        thread::spawn(move || {
            let before = migo_alloc_probe::thread_counts();
            let request = Arc::new(Request {
                context: 1,
                format: f,
                cancel: std::sync::atomic::AtomicBool::new(false),
                cpu,
            });
            let mut ready = Ready::new(request).unwrap();
            let mut stages = 0;
            while ready.is_preparing() {
                ready.step(&[], false).unwrap();
                stages += 1;
            }
            let _session = Session::allocate(
                ready,
                Limits {
                    source_ahead: MAX_AHEAD,
                    pending_events: MAX_EVENTS,
                },
            )
            .unwrap();
            let after = migo_alloc_probe::thread_counts();
            assert!(stages <= 4);
            let allocated = after.bytes_allocated - before.bytes_allocated;
            assert!(
                allocated > 100000,
                "construction allocation probe was not installed"
            );
            assert!(
                allocated + 65536 < OWNER_BYTES as u64,
                "mature native construction exceeds the reserved peak: {allocated}"
            );
        })
        .join()
        .unwrap();
    }
}
#[test]
fn dropped_preparation_and_panicking_setup_return_all_credit() {
    let scheduler = IoScheduler::new(0);
    let h = local(&scheduler);
    // Drop after one poll, including when admission is still queued.
    for _ in 0..8 {
        let h = h.clone();
        crate::streaming::get_stream_runtime().block_on(async {
            let future = h.prepare(1, format(768000, 3000, 2), limits(), None);
            let mut future = std::pin::pin!(future);
            std::future::poll_fn(|cx| {
                let _ = future.as_mut().poll(cx);
                std::task::Poll::Ready(())
            })
            .await;
        });
        empty(&h);
    }
    let hook: Hook = Arc::new(|_| panic!("intentional device setup panic"));
    assert!(
        crate::streaming::get_stream_runtime()
            .block_on(h.prepare(1, format(44100, 48000, 2), limits(), Some(hook)))
            .is_err()
    );
    empty(&h);
}
#[test]
fn invalid_formats_closed_hosts_and_counter_overflow_cannot_enter_admission() {
    let scheduler = IoScheduler::new(0);
    let h = local(&scheduler);
    for f in [
        format(0, 48000, 2),
        format(2999, 48000, 2),
        format(768001, 48000, 2),
        format(48000, 48000, 2),
        format(44100, 48000, 33),
    ] {
        assert_eq!(
            h.reserve(f).err().map(|e| e.code),
            Some(ErrorCode::InvalidArgument),
            "unsupported managed format entered memory admission"
        );
    }
    empty(&h);
    let native = Ready::new(Arc::new(Request {
        context: 1,
        format: format(48000, 48000, 2),
        cancel: std::sync::atomic::AtomicBool::new(false),
        cpu: h.cpu.clone(),
    }))
    .unwrap();
    assert_eq!(
        Session::allocate(native, limits()).err().map(|e| e.code),
        Some(ErrorCode::InvalidArgument),
        "bypass session was unwrapped before validation"
    );
    for budget in [
        Limits {
            source_ahead: 0,
            pending_events: 64,
        },
        Limits {
            source_ahead: MAX_AHEAD,
            pending_events: MAX_EVENTS + 1,
        },
    ] {
        assert_eq!(
            crate::streaming::get_stream_runtime()
                .block_on(h.prepare(1, format(44100, 48000, 2), budget, None))
                .err()
                .map(|e| e.code),
            Some(ErrorCode::InvalidArgument),
            "invalid managed pull budget was accepted"
        );
        empty(&h);
    }
    let overflow = Usage::new(usize::MAX);
    overflow.owners.store(usize::MAX, Ordering::Release);
    assert_eq!(
        overflow.reserve().err().map(|e| e.code),
        Some(ErrorCode::InputSaturated),
        "owner count wrapped at usize limit"
    );
    scheduler.close();
    assert_eq!(
        h.reserve(format(44100, 48000, 2)).err().map(|e| e.code),
        Some(EngineError::from(migo_io::pools::PoolError::Closed).code)
    );
    empty(&h);
}
