//! The same exclusive state, queued captures and unclaimed output keep credit
//! until actual CPU disposal, including after cancellation or host close.
use super::*;
use crate::limits::{AudioAggregateLedger, AudioAggregatePermit};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
};
#[derive(Default)]
struct Dropped {
    count: std::sync::atomic::AtomicUsize,
    wrong: AtomicBool,
}
fn witness(seen: &Arc<Dropped>) -> Arc<dyn Fn() + Send + Sync> {
    let seen = seen.clone();
    Arc::new(move || {
        if !std::thread::current()
            .name()
            .unwrap_or("")
            .starts_with("Migo-IO-")
        {
            seen.wrong.store(true, Ordering::Release);
        }
        seen.count.fetch_add(1, Ordering::Release);
    })
}
fn wait(mut ready: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    while !ready() {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(15),
            "offworker ownership observation did not finish"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
struct CpuBlocked {
    gate: StepGate,
    entered: Arc<AtomicBool>,
}
impl Drop for CpuBlocked {
    fn drop(&mut self) {
        self.entered.store(true, Ordering::Release);
        self.gate.hold();
    }
}
fn block(cpu: &AudioCpu) -> Release {
    let gate = StepGate::new();
    let entered = Arc::new(AtomicBool::new(false));
    let value = CpuBlocked {
        gate: gate.clone(),
        entered: entered.clone(),
    };
    let retired = cpu.retirement();
    crate::streaming::get_stream_runtime().spawn(async move {
        retired.retire(value).await;
    });
    wait(|| entered.load(Ordering::Acquire));
    Release(gate)
}
const BYTES: usize = 4 << 20;
struct Witness(Arc<dyn Fn() + Send + Sync>);
impl Drop for Witness {
    fn drop(&mut self) {
        (self.0)();
    }
}
struct Payload {
    data: Vec<u8>,
    _permit: AudioAggregatePermit,
    _witness: Witness,
}
fn payload(ledger: &Arc<AudioAggregateLedger>, seen: &Arc<Dropped>) -> Payload {
    Payload {
        data: vec![7; BYTES],
        _permit: ledger
            .try_reserve(BYTES, "offworker custody fixture")
            .unwrap(),
        _witness: Witness(witness(seen)),
    }
}
struct Woke(AtomicBool);
impl Wake for Woke {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::Release);
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.store(true, Ordering::Release);
    }
}
fn poll<F: Future>(f: Pin<&mut F>, w: &Arc<Woke>) -> Poll<F::Output> {
    f.poll(&mut Context::from_waker(&Waker::from(w.clone())))
}
fn sentinel() {
    let (tx, rx) = std::sync::mpsc::channel();
    crate::streaming::get_stream_runtime().spawn(async move {
        let _ = tx.send(());
    });
    rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
}
#[derive(Clone)]
struct StepGate(Arc<(Mutex<bool>, Condvar)>);
impl StepGate {
    fn new() -> Self {
        Self(Arc::new((Mutex::new(false), Condvar::new())))
    }
    fn open(&self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
    fn hold(&self) {
        let mut open = self.0.0.lock().unwrap();
        while !*open {
            open = self.0.1.wait(open).unwrap();
        }
    }
}
struct Release(StepGate);
impl Release {
    fn open(&self) {
        self.0.open();
    }
}
impl Drop for Release {
    fn drop(&mut self) {
        self.0.open();
    }
}
pub(super) fn custody() {
    for background in [false, true] {
        for mode in [
            "idle",
            "between",
            "unpolled",
            "queued",
            "closed",
            "full",
            "ready",
            "running",
            "running-close",
        ] {
            case(mode, background);
        }
    }
}
fn case(mode: &str, background: bool) {
    let host = migo_io::scheduler::IoScheduler::isolated_for_admission_probe(801, 1, u64::MAX);
    let peer = host.peer_for_admission_probe(802);
    let cpu = if background {
        host.background_audio_cpu()
    } else {
        host.audio_cpu()
    };
    let ledger = Arc::new(AudioAggregateLedger::new(2 * BYTES));
    let seen = Arc::new(Dropped::default());
    let value = Box::new(payload(&ledger, &seen));
    let original = value.as_ref() as *const Payload as usize;
    let mut owner = OffWorker::from_box(value, cpu.clone());
    assert_eq!(
        owner.value.as_deref().unwrap() as *const Payload as usize,
        original,
        "offworker boxed state was copied or reallocated"
    );
    if mode == "between" {
        let address = owner.value.as_deref().unwrap() as *const Payload as usize;
        let (returned, ptr) = crate::streaming::get_stream_runtime()
            .block_on(owner.with(|p| {
                p.data[0] = 19;
                p as *const Payload as usize
            }))
            .unwrap();
        owner = returned;
        assert!(
            ptr == address && owner.value.as_ref().unwrap().data[0] == 19,
            "offworker handoff changed exclusive address or mutated state"
        );
    }
    if matches!(mode, "idle" | "between") {
        let gate = block(&cpu);
        let before = migo_alloc_probe::thread_counts();
        drop(owner);
        sentinel();
        let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
        let held = ledger.used_bytes() == BYTES && seen.count.load(Ordering::Acquire) == 0;
        gate.open();
        assert!(
            freed < (BYTES / 2) as u64 && held,
            "offworker state escaped CPU retirement"
        );
        finish(&ledger, &seen, 1);
        return;
    }
    let capture = payload(&ledger, &seen);
    let ran = Arc::new(AtomicBool::new(false));
    let job_ran = ran.clone();
    let entered = Arc::new(AtomicBool::new(false));
    let job_entered = entered.clone();
    let step_gate = StepGate::new();
    let step_hold = step_gate.clone();
    let release = Release(step_gate);
    let blocking = matches!(mode, "running" | "running-close");
    let ready = mode == "ready";
    let mut gate = (!blocking && !ready).then(|| block(&cpu));
    let mut fillers = Vec::new();
    let mut original_error: Option<shared::error::EngineError> = None;
    let w = Arc::new(Woke(AtomicBool::new(false)));
    if mode == "full" {
        loop {
            let filler_cpu = cpu.clone();
            let mut f = Box::pin(async move { filler_cpu.run(|| ()).await });
            match poll(f.as_mut(), &w) {
                Poll::Pending => fillers.push(f),
                Poll::Ready(Err(e)) => {
                    original_error = Some(e);
                    break;
                }
                _ => panic!("blocked CPU unexpectedly executed queue filler"),
            }
            assert!(
                fillers.len() < 32,
                "offworker queue fill did not reach original admission limit"
            );
        }
    }
    let mut future = Box::pin(owner.with(move |p| {
        job_entered.store(true, Ordering::Release);
        if blocking {
            step_hold.hold();
        }
        p.data[0] = 23;
        job_ran.store(true, Ordering::Release);
        capture
    }));
    if mode == "closed" {
        host.close();
        original_error = Some(migo_io::pools::PoolError::Closed.into());
    }
    if mode != "unpolled" {
        match poll(future.as_mut(), &w) {
            Poll::Pending => assert!(
                !matches!(mode, "closed" | "full"),
                "offworker refused step lost original typed error"
            ),
            Poll::Ready(Err(e)) => assert!(
                matches!(mode, "closed" | "full")
                    && original_error
                        .as_ref()
                        .is_some_and(|old| e.code == old.code && e.detail == old.detail),
                "offworker refused step lost original typed error"
            ),
            Poll::Ready(Ok(_)) => {
                panic!("offworker step unexpectedly completed before controlled CPU handoff")
            }
        }
    }
    if ready {
        wait(|| w.0.load(Ordering::Acquire));
        assert!(
            ran.load(Ordering::Acquire),
            "offworker ready result never executed its CPU step"
        );
        // Completion wake follows original CPU credit publication. Do not poll
        // again: cancellation must now protect the computed, unclaimed result.
        gate = Some(block(&cpu));
    }
    if blocking {
        wait(|| entered.load(Ordering::Acquire));
        if mode == "running-close" {
            host.close();
        }
    }
    let before = migo_alloc_probe::thread_counts();
    drop(future);
    sentinel();
    let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
    let held = ledger.used_bytes() == 2 * BYTES && seen.count.load(Ordering::Acquire) == 0;
    let expected_run = ready;
    let ran_before = ran.load(Ordering::Acquire);
    if let Some(gate) = gate {
        gate.open();
    }
    drop(release);
    assert!(
        freed < (BYTES / 2) as u64 && held,
        "offworker capture or result escaped CPU retirement"
    );
    assert!(
        ran_before == expected_run,
        "cancelled queued offworker step executed or lost completion"
    );
    drop(fillers);
    finish(&ledger, &seen, 2);
    assert_eq!(
        crate::streaming::get_stream_runtime()
            .block_on(peer.audio_cpu().run(|| 42))
            .unwrap(),
        42,
        "offworker disposal closed a peer host"
    );
}
fn finish(ledger: &Arc<AudioAggregateLedger>, seen: &Arc<Dropped>, count: usize) {
    wait(|| seen.count.load(Ordering::Acquire) == count && ledger.used_bytes() == 0);
    assert!(
        !seen.wrong.load(Ordering::Acquire),
        "offworker payload destructor ran outside original CPU worker"
    );
}
