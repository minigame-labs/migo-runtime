//! Real managed owners on the same process executor as the paced player.
//! Setup and destruction are distinct windows; there are no CPU entry gates.
use super::*;
use migo_io::scheduler::IoScheduler;
use std::{
    thread,
    time::{Duration, Instant},
};

pub(crate) struct Batch {
    devices: Vec<Device>,
    hosts: Vec<Host>,
    schedulers: Vec<IoScheduler>,
    ended: Arc<AtomicUsize>,
    bad_thread: Arc<std::sync::atomic::AtomicBool>,
    setup_ns: u128,
    setup_requests: usize,
}
impl Batch {
    pub(crate) fn prepare() -> Self {
        let start = Instant::now();
        let schedulers: Vec<_> = (0..4).map(IoScheduler::new).collect();
        let generation = schedulers[0].audio_cpu().refill_generation();
        let ended = Arc::new(AtomicUsize::new(0));
        let bad_thread = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hosts: Vec<_> = schedulers
            .iter()
            .map(|scheduler| {
                let mut host = Host::new(scheduler.audio_cpu());
                let ended = ended.clone();
                let bad = bad_thread.clone();
                host.retire_hook = Some(Arc::new(move |before| {
                    if !thread::current()
                        .name()
                        .unwrap_or("")
                        .starts_with("Migo-IO-")
                    {
                        bad.store(true, Ordering::Release);
                    }
                    if !before {
                        ended.fetch_add(1, Ordering::Release);
                    }
                }));
                host
            })
            .collect();
        let formats = [
            (44_100, 48_000, 2),
            (48_000, 44_100, 2),
            (3_000, 768_000, 32),
            (768_000, 3_000, 32),
        ];
        let mut batch = Self {
            devices: Vec::with_capacity(32),
            hosts,
            schedulers,
            ended,
            bad_thread,
            setup_ns: 0,
            setup_requests: 0,
        };
        for host in &batch.hosts {
            for n in 0..8 {
                let (source, target, channels) = formats[n % formats.len()];
                let device = crate::streaming::get_stream_runtime()
                    .block_on(host.prepare(
                        1,
                        Format {
                            source,
                            target,
                            channels,
                        },
                        Limits {
                            source_ahead: MAX_AHEAD,
                            pending_events: 64,
                        },
                        None,
                    ))
                    .expect("real device filter preparation failed");
                batch.devices.push(device);
            }
        }
        assert!(
            batch
                .hosts
                .iter()
                .all(|host| host.usage.owners.load(Ordering::Acquire) == 8)
        );
        assert_eq!(batch.hosts[0].process.owners.load(Ordering::Acquire), 32);
        batch.setup_ns = start.elapsed().as_nanos();
        batch.setup_requests = batch.hosts[0]
            .cpu
            .refill_generation()
            .wrapping_sub(generation);
        batch
    }
    fn retire(mut self) -> (u128, usize, bool) {
        let generation = self.hosts[0].cpu.refill_generation();
        let start = Instant::now();
        for scheduler in &self.schedulers {
            scheduler.close();
        }
        drop(std::mem::take(&mut self.devices));
        while self.ended.load(Ordering::Acquire) < 32 {
            assert!(
                start.elapsed() < Duration::from_secs(15),
                "actual device owners did not retire"
            );
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            self.hosts
                .iter()
                .all(|host| host.usage.owners.load(Ordering::Acquire) == 0)
        );
        assert_eq!(
            self.hosts[0].process.owners.load(Ordering::Acquire),
            0,
            "retirement leaked physical owner credit"
        );
        (
            start.elapsed().as_nanos(),
            self.hosts[0]
                .cpu
                .refill_generation()
                .wrapping_sub(generation),
            self.bad_thread.load(Ordering::Acquire),
        )
    }
    pub(crate) fn discard(self) {
        let _ = self.retire();
    }
}
impl Drop for Batch {
    fn drop(&mut self) {
        for scheduler in &self.schedulers {
            scheduler.close();
        }
    }
}

#[derive(Default)]
struct Counters {
    completed: AtomicUsize,
    phase: AtomicUsize,
    retired: AtomicUsize,
    bad_thread: std::sync::atomic::AtomicBool,
}
pub(crate) struct Summary {
    pub(crate) batches: usize,
    pub(crate) retired: usize,
    pub(crate) setup_requests: usize,
    pub(crate) retirement_requests: usize,
    pub(crate) bad_thread: bool,
    windows: Vec<(u128, u128, u128, usize, usize)>,
}
impl Summary {
    pub(crate) fn json(&self) -> String {
        let windows = self.windows.iter().enumerate().map(|(index, (end, setup, retire, setup_requests, retirement_requests))| {
            format!("{{\"prewarmed_setup\":{},\"end_ns\":{end},\"setup_ns\":{setup},\"retirement_ns\":{retire},\"setup_foreground_requests\":{setup_requests},\"retirement_foreground_requests\":{retirement_requests}}}", index == 0)
        }).collect::<Vec<_>>().join(",");
        format!(
            "{{\"batches\":{},\"retired_owners\":{},\"setup_foreground_requests\":{},\"retirement_foreground_requests\":{},\"bad_destructor_thread\":{},\"windows\":[{windows}]}}",
            self.batches,
            self.retired,
            self.setup_requests,
            self.retirement_requests,
            self.bad_thread
        )
    }
}
pub(crate) struct Load {
    stop: Arc<std::sync::atomic::AtomicBool>,
    counters: Arc<Counters>,
    worker: Option<thread::JoinHandle<Summary>>,
}
impl Load {
    pub(crate) fn start(first: Batch) -> Self {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let counters = Arc::new(Counters::default());
        let stopped = stop.clone();
        let progress = counters.clone();
        let worker = thread::Builder::new()
            .name("device-churn".into())
            .spawn(move || {
                let origin = Instant::now();
                let mut next = Some(first);
                let mut summary = Summary {
                    batches: 0,
                    retired: 0,
                    setup_requests: 0,
                    retirement_requests: 0,
                    bad_thread: false,
                    windows: Vec::new(),
                };
                loop {
                    let cycle = Instant::now();
                    progress.phase.store(1, Ordering::Release);
                    let batch = next.take().unwrap_or_else(Batch::prepare);
                    let (setup_ns, setup_requests) = (batch.setup_ns, batch.setup_requests);
                    progress.phase.store(2, Ordering::Release);
                    let (retire_ns, requests, bad) = batch.retire();
                    summary.batches += 1;
                    summary.retired += 32;
                    summary.setup_requests += setup_requests;
                    summary.retirement_requests += requests;
                    summary.bad_thread |= bad;
                    summary.windows.push((
                        origin.elapsed().as_nanos(),
                        setup_ns,
                        retire_ns,
                        setup_requests,
                        requests,
                    ));
                    progress.retired.store(summary.retired, Ordering::Release);
                    progress
                        .bad_thread
                        .store(summary.bad_thread, Ordering::Release);
                    progress.completed.store(summary.batches, Ordering::Release);
                    progress.phase.store(0, Ordering::Release);
                    if stopped.load(Ordering::Acquire)
                        || origin.elapsed() >= Duration::from_secs(30)
                    {
                        break;
                    }
                    while cycle.elapsed() < Duration::from_secs(2)
                        && !stopped.load(Ordering::Acquire)
                    {
                        thread::park_timeout(Duration::from_millis(20));
                    }
                    if stopped.load(Ordering::Acquire)
                        || origin.elapsed() >= Duration::from_secs(30)
                    {
                        break;
                    }
                }
                summary
            })
            .unwrap();
        Self {
            stop,
            counters,
            worker: Some(worker),
        }
    }
    pub(crate) fn snapshot(&self) -> (usize, usize, usize, bool) {
        (
            self.counters.completed.load(Ordering::Acquire),
            self.counters.phase.load(Ordering::Acquire),
            self.counters.retired.load(Ordering::Acquire),
            self.counters.bad_thread.load(Ordering::Acquire),
        )
    }
    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = &self.worker {
            worker.thread().unpark();
        }
    }
    pub(crate) fn finish(&mut self) -> Summary {
        self.stop();
        self.worker
            .take()
            .unwrap()
            .join()
            .expect("real device churn failed")
    }
}
impl Drop for Load {
    fn drop(&mut self) {
        self.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
