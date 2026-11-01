//! Preparing32 owners used to enqueue32 ordinary Audio operations at once.
//! Hold the two real CPU entries so rejection/foreground admission is tested
//! deterministically, separately from the unbarriered cold playback probe.
use super::*;
use migo_io::scheduler::IoScheduler;
use std::{
    future::Future,
    pin::pin,
    sync::{Condvar, Mutex, mpsc},
    task::{Context, Poll, Waker},
    thread,
    time::{Duration, Instant},
};

fn host(cpu: AudioCpu, process: Arc<Usage>, aggregate: Arc<AudioAggregateLedger>) -> Host {
    Host {
        cpu: cpu.background(),
        usage: Usage::new(8),
        process,
        aggregate,
        retire_hook: None,
    }
}
fn format() -> Format {
    Format {
        source: 44100,
        target: 48000,
        channels: 2,
    }
}
fn limits() -> Limits {
    Limits {
        source_ahead: MAX_AHEAD,
        pending_events: 64,
    }
}
fn until(mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(15);
    while !f() {
        assert!(Instant::now() < end, "preparation fixture did not progress");
        thread::yield_now();
    }
}
fn empty(h: &Host) {
    until(|| {
        h.usage.owners.load(Ordering::Acquire) == 0
            && h.process.owners.load(Ordering::Acquire) == 0
            && h.aggregate.used_bytes() == 0
    });
}
struct Release(Arc<(Mutex<bool>, Condvar)>);
impl Drop for Release {
    fn drop(&mut self) {
        let (lock, wake) = &*self.0;
        *lock.lock().unwrap() = true;
        wake.notify_all();
    }
}
fn hold() -> (Hook, mpsc::Receiver<()>, Release) {
    let latch = Arc::new((Mutex::new(false), Condvar::new()));
    let (sent, entered) = mpsc::channel();
    let state = latch.clone();
    let hook: Hook = Arc::new(move |ready| {
        if ready.stream.as_ref().unwrap().prepared_layers() != 0 {
            return;
        }
        assert!(
            thread::current()
                .name()
                .unwrap_or("")
                .starts_with("Migo-IO-")
        );
        let _ = sent.send(());
        let (lock, wake) = &*state;
        let mut open = lock.lock().unwrap();
        while !*open {
            open = wake.wait(open).unwrap();
        }
    });
    (hook, entered, Release(latch))
}

pub(super) fn concurrent_burst() {
    let first = IoScheduler::isolated_for_admission_probe(0, 6, 128 * 1024 * 1024);
    let mut schedulers: Vec<_> = (1..4).map(|n| first.peer_for_admission_probe(n)).collect();
    let live = first.peer_for_admission_probe(4);
    schedulers.push(first);
    let process = Usage::new(32);
    let aggregate = Arc::new(AudioAggregateLedger::new(32 * OWNER_BYTES));
    let hosts: Vec<_> = schedulers
        .iter()
        .map(|s| host(s.audio_cpu(), process.clone(), aggregate.clone()))
        .collect();
    let (hook, entered, release) = hold();
    let polled = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for h in &hosts {
        for id in 0..8 {
            let h = h.clone();
            let hook = hook.clone();
            let polled = polled.clone();
            tasks.push(crate::streaming::get_stream_runtime().spawn(async move {
                let future = h.prepare(id, format(), limits(), Some(hook));
                let mut future = pin!(future);
                let mut first = true;
                std::future::poll_fn(|cx| {
                    let result = future.as_mut().poll(cx);
                    if first {
                        first = false;
                        polled.fetch_add(1, Ordering::Release);
                    }
                    result
                })
                .await
            }));
        }
    }
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    until(|| polled.load(Ordering::Acquire) == 32);
    let held_owners = process.owners.load(Ordering::Acquire);
    let held_bytes = aggregate.used_bytes();
    // A refill cannot execute while both class workers are held, but it must
    // still be admitted instead of being rejected behind preparation jobs.
    let cpu = live.audio_cpu();
    let mut foreground = pin!(cpu.run(|| 42_u32));
    let mut cx = Context::from_waker(Waker::noop());
    let initial = foreground.as_mut().poll(&mut cx);
    let foreground_admitted = initial.is_pending();
    drop(release);
    let foreground_result = match initial {
        Poll::Ready(result) => result,
        Poll::Pending => crate::streaming::get_stream_runtime().block_on(foreground),
    };
    let mut devices = Vec::new();
    let mut rejected = 0;
    for task in tasks {
        match crate::streaming::get_stream_runtime()
            .block_on(task)
            .unwrap()
        {
            Ok(device) => devices.push(device),
            Err(_) => rejected += 1,
        }
    }
    drop(devices);
    for h in &hosts {
        empty(h);
    }
    assert_eq!(held_owners, 32, "waiting preparation lost owner quota");
    assert_eq!(
        held_bytes,
        32 * OWNER_BYTES,
        "waiting preparation lost physical quota"
    );
    assert_eq!(
        rejected, 0,
        "preparation burst rejected an admitted managed route"
    );
    assert!(
        foreground_admitted && matches!(foreground_result, Ok(42)),
        "preparation burst denied live foreground admission"
    );
}

pub(super) fn permit_lifetime() {
    let scheduler = IoScheduler::isolated_for_admission_probe(0, 2, 32 * 1024 * 1024);
    let h = host(
        scheduler.audio_cpu(),
        Usage::new(8),
        Arc::new(AudioAggregateLedger::new(8 * OWNER_BYTES)),
    );
    let runtime = crate::streaming::get_stream_runtime();
    let device = runtime
        .block_on(h.prepare(1, format(), limits(), None))
        .unwrap();
    let mut cx = Context::from_waker(Waker::noop());
    let released = {
        let mut permit = pin!(h.cpu.preparation_permit());
        matches!(permit.as_mut().poll(&mut cx), Poll::Ready(Ok(_)))
    };
    drop(device);
    empty(&h);
    assert!(released, "completed preparation retained CPU admission");

    let (hook, entered, release) = hold();
    let owner = h.clone();
    let task = runtime.spawn(async move { owner.prepare(2, format(), limits(), Some(hook)).await });
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    task.abort();
    assert!(matches!(runtime.block_on(task), Err(error) if error.is_cancelled()));
    let held = {
        let mut permit = pin!(h.cpu.preparation_permit());
        permit.as_mut().poll(&mut cx).is_pending()
    };
    let physical = h.aggregate.used_bytes();
    drop(release);
    empty(&h);
    assert!(
        held,
        "cancelled native preparation released admission early"
    );
    assert_eq!(
        physical, OWNER_BYTES,
        "cancelled native preparation released physical credit early"
    );
    let mut reusable = pin!(h.cpu.preparation_permit());
    assert!(
        matches!(reusable.as_mut().poll(&mut cx), Poll::Ready(Ok(_))),
        "actual preparation retirement leaked CPU admission"
    );
}
