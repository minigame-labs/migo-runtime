use super::*;
use migo_io::scheduler::IoScheduler;
use std::{
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
fn host(cpu: AudioCpu, owners: usize, process: Arc<Usage>) -> Host {
    Host {
        cpu: cpu.background(),
        usage: Usage::new(owners),
        process,
    }
}
fn device(host: &Host, f: Format) -> Device {
    crate::streaming::get_stream_runtime()
        .block_on(host.prepare(1, f, limits(), None))
        .unwrap()
}
fn until(mut done: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < end, "device owner credit was not returned");
        thread::yield_now();
    }
}
fn empty(host: &Host) {
    until(|| {
        host.usage.owners.load(Ordering::Acquire) == 0
            && host.process.owners.load(Ordering::Acquire) == 0
    });
}

#[test]
fn exact_host_process_limits_and_rejection_rollback_hold() {
    let scheduler = IoScheduler::new(0);
    let process = Usage::new(3);
    let a = host(scheduler.audio_cpu(), 2, process.clone());
    let b = host(scheduler.audio_cpu(), 2, process);
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
    drop(two);
    let replacement = b.reserve(f).unwrap();
    drop((one, three, replacement));
    empty(&a);
    empty(&b);
}

#[test]
fn a_prepared_device_returns_its_owner_credit_when_dropped() {
    let scheduler = IoScheduler::new(0);
    let host = host(scheduler.audio_cpu(), 2, Usage::new(4));
    let prepared = device(&host, format(44100, 48000, 2));
    assert_eq!(host.usage.owners.load(Ordering::Acquire), 1);
    drop(prepared);
    empty(&host);
}
