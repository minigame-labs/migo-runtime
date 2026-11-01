use super::*;
use std::{
    sync::{Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

const WAIT: Duration = Duration::from_secs(10);
fn format(source: u32, target: u32, channels: u32) -> Format {
    Format {
        source,
        target,
        channels,
    }
}
fn until(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + WAIT;
    while !condition() {
        assert!(
            Instant::now() < end,
            "device preparation owner did not retire"
        );
        thread::yield_now();
    }
}
fn take(slot: &mut Slot) -> EngineResult<Box<Ready>> {
    let mut result = None;
    until(|| {
        result = slot.take();
        result.is_some()
    });
    result.unwrap()
}
fn prepared(slot: &mut Slot, host: &migo_io::scheduler::IoScheduler, f: Format) -> Box<Ready> {
    slot.request(f, host.audio_cpu(), ThreadWakeup::new(), None)
        .unwrap();
    take(slot).expect("prepared owner unavailable")
}
struct Release(Option<mpsc::Sender<()>>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.take();
    }
}
fn hold_layer(layer: usize) -> (Hook, mpsc::Receiver<()>, Release) {
    let (send, started) = mpsc::channel();
    let (release, waiting) = mpsc::channel();
    let waiting = Mutex::new(Some(waiting));
    let hook: Hook = Arc::new(move |ready| {
        if ready.stream.as_ref().unwrap().prepared_layers() == layer
            && let Some(waiting) = waiting.lock().unwrap().take()
        {
            send.send(()).unwrap();
            if thread::current()
                .name()
                .unwrap_or("")
                .starts_with("Migo-IO-")
            {
                let _ = waiting.recv_timeout(WAIT);
            }
        }
    });
    (hook, started, Release(Some(release)))
}

#[test]
fn actual_cpu_turns_prepare_one_layer_and_leave_the_shared_async_worker_free() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    let mut slot = Slot::new(7);
    let (hold, started, release) = hold_layer(0);
    let observations = Arc::new(Mutex::new(Vec::new()));
    let observed = observations.clone();
    let hook: Hook = Arc::new(move |ready| {
        observed.lock().unwrap().push((
            ready.stream.as_ref().unwrap().prepared_layers(),
            ready as *const Ready as usize,
            ready.output.as_ptr() as usize,
            thread::current().name().unwrap_or("").to_owned(),
        ));
        hold(ready);
    });
    slot.request(
        format(768000, 3000, 2),
        host.audio_cpu(),
        ThreadWakeup::new(),
        Some(hook),
    )
    .unwrap();
    started
        .recv_timeout(WAIT)
        .expect("first CPU layer did not start");

    let (send, reply) = mpsc::channel();
    crate::streaming::get_stream_runtime().spawn(async move {
        send.send(()).unwrap();
    });
    assert!(
        reply.recv_timeout(WAIT).is_ok(),
        "filter setup occupied the shared async worker"
    );
    drop(release);
    let ready = take(&mut slot).expect("prepared owner unavailable");
    let rows = observations.lock().unwrap();
    assert_eq!(
        rows.iter().map(|row| row.0).collect::<Vec<_>>(),
        [0, 1, 2, 3, 4],
        "preparation ran more than one layer per CPU turn"
    );
    assert!(
        rows.iter().all(|row| row.3.starts_with("Migo-IO-")),
        "setup ran outside the existing AudioCpu executor"
    );
    assert!(
        rows.iter()
            .all(|row| row.1 == &*ready as *const Ready as usize
                && row.2 == ready.output.as_ptr() as usize),
        "handoff copied or replaced the exclusive filter owner"
    );
}

#[test]
fn acceptance_rejects_live_foreign_cancelled_and_unprepared_results() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    let f = format(44100, 48000, 2);
    let mut slot = Slot::new(7);
    let own = prepared(&mut slot, &host, f);
    let mut foreign = Slot::new(7);
    let foreign_ready = prepared(&mut foreign, &host, f);
    assert!(!foreign_ready.request.cancel.load(Ordering::Acquire));
    assert!(
        !slot.accept(foreign_ready),
        "a live same-id same-format foreign generation was accepted"
    );
    let request = slot.request.as_ref().unwrap().clone();
    let unfinished = crate::streaming::get_stream_runtime()
        .block_on(host.audio_cpu().run(move || Ready::new(request)))
        .unwrap()
        .unwrap();
    assert!(
        !slot.accept(unfinished),
        "an unprepared filter reached the render owner"
    );
    slot.request
        .as_ref()
        .unwrap()
        .cancel
        .store(true, Ordering::Release);
    assert!(
        !slot.accept(own),
        "a cancelled matching generation was accepted"
    );
}

#[test]
fn same_format_and_aba_replacement_cancel_taken_owners_and_keep_new_identity() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    let a = format(44100, 48000, 2);
    let b = format(44100, 8000, 2);
    let mut slot = Slot::new(19);
    let mut first = prepared(&mut slot, &host, a);
    let mut second = prepared(&mut slot, &host, a);
    assert!(
        !Arc::ptr_eq(&first.request, &second.request),
        "same-format device reuse kept the old generation"
    );
    assert_eq!(
        first.step(&[], false).err().map(|error| error.code),
        Some(ErrorCode::Cancelled),
        "superseded route kept rendering"
    );
    assert!(
        first.stream.is_none() && first.output.capacity() == 0,
        "cancelled route retained filter scratch"
    );
    let middle = prepared(&mut slot, &host, b);
    let last = prepared(&mut slot, &host, a);
    assert_eq!(
        second.step(&[], false).unwrap_err().code,
        ErrorCode::Cancelled
    );
    assert!(
        !Arc::ptr_eq(&last.request, &first.request) && !Arc::ptr_eq(&last.request, &middle.request)
    );
    drop(slot);
    assert!(
        last.request.cancel.load(Ordering::Acquire),
        "slot release left a live route token"
    );
}

#[test]
fn precancelled_setup_and_closed_hosts_never_construct_a_filter() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    let request = Arc::new(Request {
        context: 1,
        format: format(44100, 48000, 2),
        cancel: AtomicBool::new(true),
        cpu: host.audio_cpu().background(),
    });
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = calls.clone();
    let hook: Hook = Arc::new(move |_| {
        count.fetch_add(1, Ordering::Relaxed);
    });
    let error = crate::streaming::get_stream_runtime()
        .block_on(prepare(request, Some(hook.clone())))
        .err()
        .unwrap();
    assert_eq!(
        error.code,
        ErrorCode::Cancelled,
        "pre-cancelled setup lost its cancellation code"
    );
    host.close();
    let mut slot = Slot::new(1);
    let expected = EngineError::from(PoolError::Closed).code;
    assert_eq!(
        slot.request(
            format(44100, 48000, 2),
            host.audio_cpu(),
            ThreadWakeup::new(),
            Some(hook)
        )
        .unwrap_err()
        .code,
        expected
    );
    assert_eq!(
        calls.load(Ordering::Relaxed),
        0,
        "cancelled or closed setup constructed a filter"
    );
    assert!(slot.request.is_none() && slot.pending.is_none());
}

#[test]
fn running_cancel_keeps_the_cpu_owner_until_return_and_preserves_other_hosts() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    let other = migo_io::scheduler::IoScheduler::new(1);
    let mut slot = Slot::new(2);
    let (hook, started, release) = hold_layer(1);
    slot.request(
        format(768000, 3000, 2),
        host.audio_cpu(),
        ThreadWakeup::new(),
        Some(hook),
    )
    .unwrap();
    let weak = Arc::downgrade(slot.request.as_ref().unwrap());
    started.recv_timeout(WAIT).unwrap();
    drop(slot);
    assert!(
        weak.upgrade().is_some(),
        "running CPU ownership disappeared before return"
    );
    host.close();
    drop(release);
    until(|| weak.upgrade().is_none());
    assert_eq!(
        crate::streaming::get_stream_runtime()
            .block_on(other.audio_cpu().run(|| 42))
            .unwrap(),
        42,
        "route cancellation closed another host's executor"
    );
}

#[test]
fn setup_panic_and_host_close_publish_typed_failures_without_poisoning_cpu() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    let mut slot = Slot::new(3);
    slot.request(
        format(44100, 48000, 2),
        host.audio_cpu(),
        ThreadWakeup::new(),
        Some(Arc::new(|_| panic!("injected device setup panic"))),
    )
    .unwrap();
    assert_eq!(
        take(&mut slot).err().unwrap().code,
        ErrorCode::Internal,
        "setup panic lost its typed result"
    );
    assert_eq!(
        crate::streaming::get_stream_runtime()
            .block_on(host.audio_cpu().run(|| 9))
            .unwrap(),
        9
    );
    let (hook, started, release) = hold_layer(1);
    slot.request(
        format(768000, 3000, 2),
        host.audio_cpu(),
        ThreadWakeup::new(),
        Some(hook),
    )
    .unwrap();
    started.recv_timeout(WAIT).unwrap();
    host.close();
    drop(release);
    assert_eq!(
        take(&mut slot).err().unwrap().code,
        EngineError::from(PoolError::Closed).code,
        "closed host published a ready filter"
    );
}

#[test]
fn native_bypass_uses_no_cpu_and_invalid_formats_preserve_the_live_route() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    let mut slot = Slot::new(4);
    let wakeup = ThreadWakeup::new();
    slot.request(
        format(48000, 48000, 32),
        host.audio_cpu(),
        wakeup.clone(),
        Some(Arc::new(|_| panic!("native route submitted CPU work"))),
    )
    .unwrap();
    assert!(
        slot.pending.is_none(),
        "native bypass started an async preparation"
    );
    assert!(wakeup.wait_timeout(WAIT));
    let ready = slot.take().unwrap().unwrap();
    assert!(ready.is_bypass() && ready.is_ready() && ready.output.capacity() == 0);
    for f in [
        format(0, 48000, 2),
        format(48000, 0, 2),
        format(48000, 44100, 0),
        format(48000, 44100, 33),
    ] {
        assert_eq!(
            slot.request(f, host.audio_cpu(), ThreadWakeup::new(), None)
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
        assert!(
            Arc::ptr_eq(slot.request.as_ref().unwrap(), &ready.request)
                && !ready.request.cancel.load(Ordering::Acquire),
            "invalid format retired the valid route"
        );
    }
}

#[test]
fn handed_off_filter_allocates_neither_on_first_processing_step_nor_steady_state() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    for f in [
        format(44100, 48000, 2),
        format(48000, 44100, 2),
        format(768000, 3000, 32),
        format(3000, 768000, 32),
    ] {
        let mut slot = Slot::new(5);
        let mut ready = prepared(&mut slot, &host, f);
        let input = vec![0.25; 128 * f.channels as usize];
        let mut kernels = 0;
        bandlimited::take_trace();
        for _ in 0..512 {
            let before = migo_alloc_probe::thread_counts();
            let step = ready.step(&input, false).unwrap();
            let after = migo_alloc_probe::thread_counts();
            assert_eq!(
                after.allocations, before.allocations,
                "prepared render step allocated"
            );
            assert_eq!(
                after.reallocations, before.reallocations,
                "prepared render step reallocated"
            );
            let trace = bandlimited::take_trace();
            assert!(
                trace.calls <= 1 && trace.input_samples <= 8192 && trace.output_samples <= 8192
            );
            assert!(step.consumed <= input.len() && ready.output.len() <= 8192);
            kernels += trace.calls;
        }
        assert!(
            kernels > 0,
            "allocation check never processed a real kernel"
        );
    }
}

#[test]
fn prepared_handoff_preserves_every_library_sample_and_the_original_eof_clock() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    for f in [
        format(44100, 48000, 1),
        format(48000, 44100, 2),
        format(96000, 8000, 2),
        format(768000, 3000, 32),
        format(3000, 768000, 32),
    ] {
        let frames = if f.target > 4 * f.source { 17 } else { 513 };
        let audio = crate::decoder::DecodedAudio {
            sample_rate: f.source,
            channels: f.channels,
            samples: (0..frames * f.channels as usize)
                .map(|n| ((n * 17) % 251) as f32 / 128.0 - 0.9)
                .collect(),
        };
        let reference = crate::resample_job::tests::reference(&audio, f.target);
        for partition in [1, 137, 4096] {
            let mut slot = Slot::new(6);
            let mut ready = prepared(&mut slot, &host, f);
            let mut at = 0;
            let mut result = Vec::new();
            let mut finished = false;
            for _ in 0..100000 {
                let end = (at + partition * f.channels as usize).min(audio.samples.len());
                let step = ready
                    .step(&audio.samples[at..end], at == audio.samples.len())
                    .unwrap();
                at += step.consumed;
                result.extend_from_slice(&ready.output);
                if step.progress == bandlimited::Progress::Finished {
                    finished = true;
                    break;
                }
            }
            assert!(
                finished && at == audio.samples.len(),
                "prepared handoff lost its terminal clock"
            );
            assert_eq!(
                result, reference,
                "prepared handoff changed mature PCM or the submitted format"
            );
            assert_eq!(
                result.len() / f.channels as usize,
                (frames * f.target as usize).div_ceil(f.source as usize)
            );
        }
    }
}

#[test]
fn publication_wakes_the_owner_and_pending_polls_do_not_construct_filters() {
    let host = migo_io::scheduler::IoScheduler::new(0);
    let mut slot = Slot::new(8);
    let wakeup = ThreadWakeup::new();
    let (hook, started, release) = hold_layer(1);
    slot.request(
        format(44100, 48000, 2),
        host.audio_cpu(),
        wakeup.clone(),
        Some(hook),
    )
    .unwrap();
    started.recv_timeout(WAIT).unwrap();
    for _ in 0..1000 {
        assert!(
            slot.take().is_none(),
            "pending poll fabricated a ready filter"
        );
    }
    drop(release);
    until(|| slot.pending.as_ref().unwrap().task.is_finished());
    assert!(
        wakeup.wait_timeout(Duration::ZERO),
        "preparation result did not wake the render owner"
    );
    let ready = take(&mut slot).expect("prepared owner unavailable");
    assert!(ready.is_ready() && !ready.is_preparing());
    assert!(
        slot.pending.is_none() && slot.take().is_none(),
        "prepared filter was published twice"
    );
}

#[test]
fn completed_and_taken_results_observe_host_close_before_render_use() {
    let expected = EngineError::from(PoolError::Closed).code;
    let host = migo_io::scheduler::IoScheduler::new(0);
    let mut slot = Slot::new(9);
    slot.request(
        format(44100, 48000, 2),
        host.audio_cpu(),
        ThreadWakeup::new(),
        None,
    )
    .unwrap();
    until(|| slot.pending.as_ref().unwrap().task.is_finished());
    host.close();
    assert_eq!(
        take(&mut slot).err().map(|error| error.code),
        Some(expected),
        "completed result crossed host close before acceptance"
    );

    let second = migo_io::scheduler::IoScheduler::new(1);
    let mut slot = Slot::new(9);
    let mut ready = prepared(&mut slot, &second, format(44100, 48000, 2));
    second.close();
    assert_eq!(
        ready.step(&[], false).err().map(|error| error.code),
        Some(expected),
        "handed-off filter ran after host close"
    );
    assert!(
        ready.stream.is_none() && ready.output.capacity() == 0,
        "closed host retained handed-off filter scratch"
    );
}
