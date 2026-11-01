//! Delivered, pending and cached PCM keep their original allocation through CPU
//! disposal; a healthy loan returns empty, at the same address, without growth.
use super::*;
use std::sync::{Condvar, Mutex, atomic::AtomicUsize};
use std::{
    pin::Pin,
    task::{Context, Poll, Waker},
};
const BYTES: usize = 4 << 20;
#[derive(Default)]
struct Dropped {
    count: AtomicUsize,
    wrong: AtomicBool,
}
fn observe(chunk: &mut PcmChunk, seen: &Arc<Dropped>) {
    let seen = seen.clone();
    chunk.buffer.observe(Arc::new(move || {
        if !std::thread::current()
            .name()
            .unwrap_or("")
            .starts_with("Migo-IO-")
        {
            seen.wrong.store(true, Ordering::Release);
        }
        seen.count.fetch_add(1, Ordering::Release);
    }));
}
fn wait(mut ready: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    while !ready() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "PCM retirement observation did not finish"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
struct Gate(Arc<(Mutex<bool>, Condvar)>);
impl Gate {
    fn open(&self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
}
impl Drop for Gate {
    fn drop(&mut self) {
        self.open();
    }
}
struct Blocked {
    gate: Arc<(Mutex<bool>, Condvar)>,
    entered: Arc<AtomicBool>,
}
impl Drop for Blocked {
    fn drop(&mut self) {
        self.entered.store(true, Ordering::Release);
        let mut open = self.gate.0.lock().unwrap();
        while !*open {
            open = self.gate.1.wait(open).unwrap();
        }
    }
}
fn block(cpu: &migo_io::audio_cpu::AudioCpu) -> Gate {
    let gate = Gate(Arc::new((Mutex::new(false), Condvar::new())));
    let entered = Arc::new(AtomicBool::new(false));
    let value = Blocked {
        gate: gate.0.clone(),
        entered: entered.clone(),
    };
    let retired = cpu.retirement();
    get_stream_runtime().spawn(async move {
        retired.retire(value).await;
    });
    wait(|| entered.load(Ordering::Acquire));
    gate
}
fn sample(i: usize) -> f32 {
    if i == 0 {
        -0.0
    } else {
        (i % 31) as f32 / 32.0 - 0.5
    }
}
fn loan(pool: &mut PcmPool, seen: &Arc<Dropped>, empty: bool) -> PcmChunk {
    let mut pcm = pool.take();
    pcm.buffer_mut().reserve_exact(BYTES / 4);
    if !empty {
        pcm.buffer_mut().extend((0..128).map(sample));
    }
    assert_eq!(pcm.buffer.capacity() * 4, BYTES);
    observe(&mut pcm, seen);
    pcm
}
fn cpu() -> (
    migo_io::scheduler::IoScheduler,
    migo_io::audio_cpu::AudioCpu,
) {
    let host = migo_io::scheduler::IoScheduler::isolated_for_admission_probe(901, 1, u64::MAX);
    let cpu = host.audio_cpu();
    (host, cpu)
}
fn finish(seen: &Arc<Dropped>, count: usize) {
    wait(|| seen.count.load(Ordering::Acquire) == count);
    assert!(
        !seen.wrong.load(Ordering::Acquire),
        "managed PCM destructor escaped CPU retirement"
    );
}
fn sentinel() {
    let (tx, rx) = std::sync::mpsc::channel();
    get_stream_runtime().spawn(async move {
        let _ = tx.send(());
    });
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
}
fn pending<F: Future>(f: Pin<&mut F>) -> Poll<F::Output> {
    f.poll(&mut Context::from_waker(Waker::noop()))
}
pub(super) fn retirement() {
    for closed in [false, true] {
        for mode in ["cache", "late", "overflow", "empty", "queued-samples"] {
            disposal(mode, closed);
        }
        for mode in [
            "cancel",
            "consumer-close",
            "invalid-format",
            "changed-format",
            "extent",
        ] {
            publication_refusal(mode, closed);
        }
    }
    production_driver(false);
    production_driver(true);
    recycle();
    bounded_loans();
    successful();
    legacy();
}
fn disposal(mode: &str, closed: bool) {
    let (host, cpu) = cpu();
    let peer = host.peer_for_admission_probe(902);
    let mut pool = PcmPool::with_cpu(cpu.clone());
    let seen = Arc::new(Dropped::default());
    let count = match mode {
        "overflow" => STREAM_CHANNEL_CAPACITY + 2,
        "queued-samples" => STREAM_CHANNEL_CAPACITY,
        _ => 1,
    };
    let mut chunks = Vec::new();
    for _ in 0..count {
        chunks.push(loan(&mut pool, &seen, mode == "empty"));
    }
    let (tx, mut rx) = stream_channel();
    if mode == "queued-samples" {
        for pcm in chunks.drain(..) {
            tx.try_send(StreamMsg::Samples {
                revision: StreamRevision::INITIAL,
                pcm,
            })
            .unwrap_or_else(|_| panic!("PCM fixture queue capacity changed"));
        }
        assert!(tx.try_reserve().is_err());
    }
    let gate = block(&cpu);
    if closed {
        host.close();
    }
    let before = migo_alloc_probe::thread_counts();
    match mode {
        "cache" | "empty" => {
            drop(chunks);
            drop(pool);
        }
        "late" => {
            drop(pool);
            drop(chunks);
        }
        "overflow" => {
            drop(chunks);
            drop(pool);
        }
        _ => {
            drop(pool);
            rx.close();
            drop(rx);
        }
    }
    sentinel();
    let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
    let held = seen.count.load(Ordering::Acquire) == 0;
    gate.open();
    assert!(
        freed < (BYTES / 2) as u64 && held,
        "managed PCM buffer freed before CPU disposal"
    );
    finish(&seen, count);
    assert_eq!(
        get_stream_runtime()
            .block_on(peer.audio_cpu().run(|| 42))
            .unwrap(),
        42,
        "PCM disposal closed peer host"
    );
}
fn publication_refusal(mode: &str, closed: bool) {
    let (host, cpu) = cpu();
    let mut pool = PcmPool::with_cpu(cpu.clone());
    let seen = Arc::new(Dropped::default());
    let pcm = loan(&mut pool, &seen, false);
    let (tx, mut rx) = stream_channel();
    let state = StreamingState::new();
    let mut publication = StreamPublication::default();
    if matches!(mode, "cancel" | "consumer-close") {
        for _ in 0..STREAM_CHANNEL_CAPACITY {
            tx.try_send(StreamMsg::Done {
                revision: StreamRevision::INITIAL,
            })
            .unwrap_or_else(|_| panic!("PCM fixture full channel"));
        }
    }
    if mode == "changed-format" {
        publication.format = Some((44100, 1));
    }
    if mode == "extent" {
        publication
            .bind_timeline(StreamTimeline::new(1, 48000, 48000, 2).unwrap(), 0)
            .unwrap();
    }
    let gate = block(&cpu);
    if closed {
        host.close();
    }
    let before = migo_alloc_probe::thread_counts();
    let channels = if mode == "invalid-format" { 0 } else { 2 };
    let mut future = Box::pin(publish_stream_chunk(
        &tx,
        &state,
        pcm,
        48000,
        channels,
        &mut publication,
    ));
    let result = if matches!(mode, "cancel" | "consumer-close") {
        assert!(
            pending(future.as_mut()).is_pending(),
            "PCM publication failed to wait at original channel capacity"
        );
        if mode == "cancel" {
            state.cancel();
        } else {
            rx.close();
        }
        match pending(future.as_mut()) {
            Poll::Ready(r) => r,
            _ => panic!("PCM publication cancellation did not wake"),
        }
    } else {
        match pending(future.as_mut()) {
            Poll::Ready(r) => r,
            _ => panic!("PCM validation unexpectedly waited"),
        }
    };
    drop(future);
    drop(pool);
    sentinel();
    let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
    let held = seen.count.load(Ordering::Acquire) == 0;
    let semantic = match mode {
        "cancel" | "consumer-close" => result.is_ok_and(|v| !v),
        "invalid-format" => result.is_err_and(|e| {
            e.code == ErrorCode::Internal
                && e.detail.as_deref() == Some("decoded audio samples had no complete PCM format")
        }),
        "changed-format" => result.is_err_and(|e| {
            e.code == ErrorCode::IoError && e.detail.as_deref() == Some("stream PCM format changed")
        }),
        _ => result.is_err_and(|e| {
            e.code == ErrorCode::IoError
                && e.detail.as_deref() == Some("PCM exceeds publication timeline")
        }),
    };
    let mut samples = false;
    while let Ok(message) = rx.try_recv() {
        samples |= matches!(message, StreamMsg::Samples { .. });
    }
    gate.open();
    assert!(
        freed < (BYTES / 2) as u64 && held,
        "cancelled or refused publication PCM escaped CPU disposal"
    );
    assert!(
        semantic && !samples,
        "PCM publication changed cancellation, typed error or message order"
    );
    finish(&seen, 1);
}
fn recycle() {
    let (_host, cpu) = cpu();
    let mut pool = PcmPool::with_cpu(cpu.clone());
    let seen = Arc::new(Dropped::default());
    let pcm = loan(&mut pool, &seen, false);
    let address = pcm.buffer.as_ptr() as usize;
    drop(pcm);
    let gate = block(&cpu);
    migo_alloc_probe::assert_no_steady_state_allocation(
        migo_alloc_probe::Burst {
            path: "managed PCM return/acquire",
            warmup: 40,
            measured: 128,
        },
        |_| {
            let mut acquiring = std::pin::pin!(pool.acquire());
            let Poll::Ready(mut pcm) = pending(acquiring.as_mut()) else {
                panic!("healthy PCM cache did not immediately satisfy admission");
            };
            assert!(
                pcm.is_empty()
                    && pcm.buffer.as_ptr() as usize == address
                    && pcm.buffer.capacity() * 4 == BYTES,
                "managed PCM recycling changed pointer, capacity or empty loan"
            );
            pcm.buffer_mut().extend((0..128).map(sample));
        },
    );
    assert_eq!(
        seen.count.load(Ordering::Acquire),
        0,
        "healthy PCM recycle retired a live allocation"
    );
    drop(pool);
    let held = seen.count.load(Ordering::Acquire) == 0;
    gate.open();
    assert!(held, "cached PCM escaped CPU disposal");
    finish(&seen, 1);
}
fn successful() {
    let (_host, cpu) = cpu();
    let mut pool = PcmPool::with_cpu(cpu.clone());
    let seen = Arc::new(Dropped::default());
    let (tx, mut rx) = stream_channel();
    let state = StreamingState::new();
    let mut publication = StreamPublication::default();
    let mut pointer = None;
    for turn in 0..2 {
        let revision = if turn == 0 {
            StreamRevision::INITIAL
        } else {
            let revision = StreamRevision::INITIAL.checked_next().unwrap();
            publication.begin(revision, &state).unwrap();
            revision
        };
        let pcm = if turn == 0 {
            loan(&mut pool, &seen, false)
        } else {
            let mut pcm = pool.take();
            pcm.buffer_mut().extend((0..128).map(sample));
            pcm
        };
        let address = pcm.buffer.as_ptr() as usize;
        if let Some(old) = pointer {
            assert_eq!(address, old, "PCM publication lost recycled allocation");
        }
        pointer = Some(address);
        get_stream_runtime().block_on(async {
            assert!(
                publish_stream_chunk(&tx, &state, pcm, 48000, 2, &mut publication)
                    .await
                    .unwrap()
            );
            assert!(publication.finish(&tx, &state, true, 0).await.unwrap());
        });
        assert!(
            matches!(rx.try_recv(),Ok(StreamMsg::Ready{revision:r,sample_rate:48000,channels:2}) if r==revision),
            "PCM publication changed Ready format or revision"
        );
        let Ok(StreamMsg::Samples { revision: r, pcm }) = rx.try_recv() else {
            panic!("PCM publication missing Samples");
        };
        assert!(
            r == revision
                && pcm.buffer.as_ptr() as usize == address
                && pcm.len() == 128
                && pcm
                    .iter()
                    .enumerate()
                    .all(|(i, v)| v.to_bits() == sample(i).to_bits()),
            "PCM publication changed complete samples or buffer ownership"
        );
        drop(pcm);
        assert!(
            matches!(rx.try_recv(),Ok(StreamMsg::Done{revision:r}) if r==revision)
                && state.download_complete.load(Ordering::Acquire),
            "PCM publication changed EOF order"
        );
    }
    let gate = block(&cpu);
    drop(pool);
    let held = seen.count.load(Ordering::Acquire) == 0;
    gate.open();
    assert!(held, "successful publication PCM escaped CPU disposal");
    finish(&seen, 1);
}
fn legacy() {
    let mut pool = PcmPool::new();
    let seen = Arc::new(Dropped::default());
    let pcm = loan(&mut pool, &seen, false);
    drop(pool);
    drop(pcm);
    assert_eq!(
        seen.count.load(Ordering::Acquire),
        1,
        "legacy PCM ownership changed"
    );
}

struct FixtureDecoder;
impl crate::streaming_transport::Decoder for FixtureDecoder {
    fn step(
        &mut self,
        input: &[u8],
        eof: bool,
        pcm: &mut Vec<f32>,
        _: &AtomicBool,
    ) -> EngineResult<crate::streaming_transport::Step> {
        pcm.clear();
        if !input.is_empty() {
            pcm.reserve_exact(BYTES / 4);
            pcm.extend((0..128).map(sample));
        }
        Ok(crate::streaming_transport::Step {
            consumed: input.len(),
            progress: if eof {
                crate::streaming_transport::Progress::Finished
            } else {
                crate::streaming_transport::Progress::NeedInput
            },
            sample_rate: 48000,
            channels: 2,
        })
    }
}
struct FixtureSource(bool);
impl crate::streaming_transport::Source<FixtureDecoder> for FixtureSource {
    type Request = ();
    fn pending(_: &FixtureDecoder) -> EngineResult<()> {
        Err(EngineError::new(ErrorCode::Internal))
    }
    async fn read(
        &mut self,
        _: &mpsc::Sender<StreamMsg>,
        _: &Arc<StreamingState>,
    ) -> EngineResult<crate::streaming_transport::SourceRead<impl AsRef<[u8]> + Send + 'static>>
    {
        if self.0 {
            Ok(crate::streaming_transport::SourceRead::End)
        } else {
            self.0 = true;
            Ok(crate::streaming_transport::SourceRead::Data(vec![1]))
        }
    }
    async fn resume(
        &mut self,
        _: (),
        _: crate::off_worker::OffWorker<Option<FixtureDecoder>>,
        _: &mpsc::Sender<StreamMsg>,
        _: &Arc<StreamingState>,
    ) -> EngineResult<Option<crate::streaming_transport::SourceResume<FixtureDecoder>>> {
        Err(EngineError::new(ErrorCode::Internal))
    }
}
fn production_driver(closed: bool) {
    let (host, cpu) = cpu();
    let (tx, mut rx) = stream_channel();
    let state = StreamingState::new();
    get_stream_runtime()
        .block_on(crate::streaming_transport::run_source(
            cpu.clone(),
            FixtureSource(false),
            None,
            &tx,
            &state,
            |_| FixtureDecoder,
        ))
        .unwrap();
    assert!(
        matches!(
            rx.try_recv(),
            Ok(StreamMsg::Ready {
                sample_rate: 48000,
                channels: 2,
                ..
            })
        ),
        "production PCM driver changed Ready"
    );
    let Ok(StreamMsg::Samples { mut pcm, .. }) = rx.try_recv() else {
        panic!("production PCM driver lost Samples");
    };
    assert!(
        pcm.len() == 128
            && pcm
                .iter()
                .enumerate()
                .all(|(i, v)| v.to_bits() == sample(i).to_bits()),
        "production PCM driver changed complete PCM"
    );
    assert!(
        matches!(rx.try_recv(), Ok(StreamMsg::Done { .. })),
        "production PCM driver changed EOF"
    );
    let seen = Arc::new(Dropped::default());
    observe(&mut pcm, &seen);
    let gate = block(&cpu);
    if closed {
        host.close();
    }
    let before = migo_alloc_probe::thread_counts();
    drop(pcm);
    sentinel();
    let freed = migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed;
    let held = seen.count.load(Ordering::Acquire) == 0;
    gate.open();
    assert!(
        freed < (BYTES / 2) as u64 && held,
        "production PCM loan bypassed CPU disposal"
    );
    finish(&seen, 1);
}

fn bounded_loans() {
    let (_host, cpu) = cpu();
    let mut pool = PcmPool::with_cpu(cpu.clone());
    let slots = pool.slots.as_ref().unwrap().clone();
    let seen = Arc::new(Dropped::default());
    let count = 2 * STREAM_CHANNEL_CAPACITY + 4;
    let mut chunks = Vec::new();
    for _ in 0..count {
        chunks.push(loan(&mut pool, &seen, false));
    }
    assert_eq!(
        slots.available_permits(),
        0,
        "PCM physical loans exceeded original window-derived bound"
    );
    let gate = block(&cpu);
    drop(chunks);
    assert_eq!(
        slots.available_permits(),
        0,
        "PCM physical loan credit returned before actual CPU disposal"
    );
    let mut held = Vec::new();
    for _ in 0..STREAM_CHANNEL_CAPACITY + 1 {
        held.push(pool.take());
    }
    let (tx, _rx) = stream_channel();
    let state = StreamingState::new();
    let mut waiting = Box::pin(crate::streaming_transport::while_active(
        &tx,
        &state,
        pool.acquire(),
    ));
    assert!(
        pending(waiting.as_mut()).is_pending(),
        "PCM retired loans allowed unbounded replacement allocation"
    );
    state.cancel();
    assert!(
        matches!(pending(waiting.as_mut()), Poll::Ready(None)),
        "PCM loan admission ignored producer cancellation"
    );
    drop(waiting);
    let returned = held.pop().unwrap();
    let address = returned.buffer.as_ptr();
    let mut waiting = Box::pin(pool.acquire());
    assert!(
        pending(waiting.as_mut()).is_pending(),
        "PCM retired loans allowed unbounded replacement allocation"
    );
    drop(returned);
    let next = match pending(waiting.as_mut()) {
        Poll::Ready(pcm) => pcm,
        _ => panic!("PCM returned buffer did not wake physical admission"),
    };
    drop(waiting);
    assert_eq!(
        next.buffer.as_ptr(),
        address,
        "PCM admission allocated instead of reusing returned buffer"
    );
    held.push(next);
    assert_eq!(seen.count.load(Ordering::Acquire), 0);
    gate.open();
    let retired = count - (STREAM_CHANNEL_CAPACITY + 1);
    wait(|| seen.count.load(Ordering::Acquire) == retired && slots.available_permits() == retired);
    drop(pool);
    drop(held);
    finish(&seen, count);
    assert_eq!(
        slots.available_permits(),
        count,
        "PCM physical slots did not return after destruction"
    );
}
