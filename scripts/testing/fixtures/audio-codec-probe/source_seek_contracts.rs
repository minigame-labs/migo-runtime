//! Real pinned file/HTTP seek, retaining decoder/source ownership after EOF.
use crate::{
    decoder_seek_contracts::{assert_pcm, resample},
    indexed_seek::{self, SeekContext, SeekSource, SeekStatus},
    isomp4_file::{FileReader, IndexedFile},
    isomp4_source_contracts::{Config, Fault, Server, client, fixture, start_server},
    isomp4_transport::IndexedHttp,
    off_worker::OffWorker,
    push_decoder::{M4a, M4aSeekDecoder, M4aSourceDecoder, M4aSourceRequest},
    resampled_streaming::Batched,
    streaming::{PcmPool, StreamMsg, StreamingState, stream_channel},
    streaming_transport::{Decoder, HttpSource, Progress, Source, SourceRead, Step, while_active},
};
use shared::error::EngineResult;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;
use symphonia::core::{
    errors::{Result as FormatResult, SeekErrorKind},
    formats::SeekedTo,
    units::Timestamp,
};
use symphonia_format_isomp4::streaming::{AtomSpan, AudioIndex};
use tokio::{
    sync::{Notify, mpsc},
    time::Instant,
};

#[derive(Default)]
struct Trace {
    seeks: AtomicUsize,
    steps: AtomicUsize,
    drops: AtomicUsize,
    seek_entered: Notify,
    dropped: Notify,
    block_seek: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    bad_consumed: AtomicBool,
    fail_seek: AtomicBool,
    request_override: Mutex<Option<M4aSourceRequest>>,
}
struct Observed<const B: bool> {
    decoder: Batched<1, M4a<B>>,
    trace: Arc<Trace>,
    io_thread: std::thread::ThreadId,
}
impl<const B: bool> Drop for Observed<B> {
    fn drop(&mut self) {
        self.trace.drops.fetch_add(1, Ordering::Relaxed);
        self.trace.dropped.notify_one();
    }
}
impl<const B: bool> Decoder for Observed<B> {
    fn step(
        &mut self,
        input: &[u8],
        eof: bool,
        out: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<Step> {
        assert_ne!(
            std::thread::current().id(),
            self.io_thread,
            "decode ran on IO thread"
        );
        self.trace.steps.fetch_add(1, Ordering::Relaxed);
        let mut step = self.decoder.step(input, eof, out, cancel)?;
        if self.trace.bad_consumed.load(Ordering::Relaxed) {
            step.consumed = 1;
        }
        Ok(step)
    }
}
impl<const B: bool> M4aSourceDecoder for Observed<B> {
    fn source_request(&self) -> Option<M4aSourceRequest> {
        self.trace
            .request_override
            .lock()
            .unwrap()
            .or_else(|| self.decoder.source_request())
    }
    fn install_index_with_length(
        &mut self,
        index: AudioIndex,
        length: Option<u64>,
    ) -> Result<(), String> {
        assert_ne!(
            std::thread::current().id(),
            self.io_thread,
            "index installed on IO thread"
        );
        self.decoder.install_index_with_length(index, length)
    }
    fn resume_at(&mut self, media: AtomSpan, offset: u64) -> Result<(), String> {
        assert_ne!(
            std::thread::current().id(),
            self.io_thread,
            "resume ran on IO thread"
        );
        self.decoder.resume_at(media, offset)
    }
}
impl<const B: bool> M4aSeekDecoder for Observed<B> {
    fn seek(&mut self, target: Timestamp) -> FormatResult<SeekedTo> {
        assert_ne!(
            std::thread::current().id(),
            self.io_thread,
            "seek ran on IO thread"
        );
        self.trace.seeks.fetch_add(1, Ordering::Relaxed);
        self.trace.seek_entered.notify_one();
        if let Some(block) = self.trace.block_seek.lock().unwrap().take() {
            block.recv_timeout(Duration::from_secs(3)).unwrap();
        }
        if self.trace.fail_seek.load(Ordering::Relaxed) {
            return Err(symphonia::core::errors::Error::DecodeError(
                "corrupt seek index",
            ));
        }
        self.decoder.seek(target)
    }
}
async fn decoder<const B: bool>(
    owner: &migo_io::scheduler::IoScheduler,
    len: u64,
    target: u32,
    trace: Arc<Trace>,
) -> OffWorker<Option<Observed<B>>> {
    let io_thread = std::thread::current().id();
    let (decoder, ()) = OffWorker::new(None, owner.audio_cpu())
        .with(move |slot| {
            *slot = Some(Observed {
                decoder: Batched::with_object_len(target, Some(len)),
                trace,
                io_thread,
            });
        })
        .await
        .unwrap();
    decoder
}
async fn http<const B: bool>(data: &[u8], config: Config) -> (Server, IndexedHttp) {
    let server = start_server(data.to_vec(), config).await;
    let client = client();
    let request = <IndexedHttp as HttpSource<Observed<B>>>::request(client.get(
        crate::network::gate::GateKind::AudioStream,
        server.url.clone(),
    ));
    let response = request.send().await.unwrap();
    let source = <IndexedHttp as HttpSource<Observed<B>>>::new(client, response).unwrap();
    (server, source)
}
async fn file(
    name: &str,
    owner: &migo_io::scheduler::IoScheduler,
) -> (IndexedFile, Arc<AtomicUsize>) {
    let dir = std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap());
    let reader = FileReader::open(std::fs::File::open(dir.join(name)).unwrap(), owner)
        .await
        .unwrap();
    let reads = reader.io_reads.clone();
    (IndexedFile::from_reader(reader), reads)
}

// Test collector only: the actual source, off-worker decoder, loans and resume
// protocol are shared with the production transport, without a second codec.
async fn drain<const B: bool, S: Source<Observed<B>>>(
    source: &mut S,
    mut decoder: OffWorker<Option<Observed<B>>>,
    pool: &mut PcmPool,
    tx: &mpsc::Sender<StreamMsg>,
    state: &Arc<StreamingState>,
    mut drain_first: bool,
) -> OffWorker<Option<Observed<B>>> {
    'read: for _ in 0..10000 {
        let (mut input, eof) = if drain_first {
            drain_first = false;
            // These seeks reuse a source already drained through EOF. A
            // cached resume keeps that flag along with its old input suffix.
            (None, true)
        } else {
            let next = tokio::time::timeout(Duration::from_secs(3), source.read(tx, state))
                .await
                .unwrap()
                .unwrap();
            match next {
                SourceRead::Data(chunk) => (Some(chunk), false),
                SourceRead::End => (None, true),
                SourceRead::Cancelled => panic!("unexpected cancellation"),
            }
        };
        let mut offset = 0;
        loop {
            let mut pcm = pool.take();
            let active = state.clone();
            let (returned, (next, out, step, request)) = decoder
                .with(move |slot| {
                    let value = slot.as_mut().unwrap();
                    let bytes = input.as_ref().map_or(&[][..], |v| &v.as_ref()[offset..]);
                    let step = value
                        .step(bytes, eof, pcm.buffer_mut(), &active.cancelled)
                        .unwrap();
                    let req = if step.progress == Progress::NeedSource {
                        Some(S::pending(value).unwrap())
                    } else {
                        None
                    };
                    (input, pcm, step, req)
                })
                .await
                .unwrap();
            decoder = returned;
            input = next;
            let remaining = input.as_ref().map_or(0, |v| v.as_ref().len()) - offset;
            assert!(step.consumed <= remaining);
            offset += step.consumed;
            COLLECT.with(|v| v.borrow_mut().extend_from_slice(&out));
            drop(out);
            match step.progress {
                Progress::Finished => return decoder,
                Progress::Cancelled => panic!("unexpected cancellation"),
                Progress::NeedInput => {
                    assert!(!eof);
                    assert_eq!(offset, input.as_ref().map_or(0, |v| v.as_ref().len()));
                    continue 'read;
                }
                Progress::Yield => tokio::task::yield_now().await,
                Progress::NeedSource => {
                    let result = source
                        .resume(request.unwrap(), decoder, tx, state)
                        .await
                        .unwrap()
                        .unwrap();
                    decoder = result.decoder;
                    if result.restart {
                        continue 'read;
                    }
                }
            }
        }
    }
    panic!("source driver did not make bounded progress")
}
thread_local! { static COLLECT: std::cell::RefCell<Vec<f32>>=const{std::cell::RefCell::new(Vec::new())}; }
fn take_pcm() -> Vec<f32> {
    COLLECT.with(|v| std::mem::take(&mut *v.borrow_mut()))
}

async fn matrix<const B: bool, S: SeekSource<Observed<B>>>(
    source: &mut S,
    owner: &migo_io::scheduler::IoScheduler,
    data: &[u8],
    target_rate: u32,
) -> usize {
    take_pcm();
    let trace = Arc::new(Trace::default());
    let state = StreamingState::new();
    let (tx, _rx) = stream_channel();
    let mut pool = PcmPool::new();
    let mut d = decoder::<B>(owner, data.len() as u64, target_rate, trace.clone()).await;
    d = drain(source, d, &mut pool, &tx, &state, false).await;
    take_pcm();
    let full = crate::candidate::decode(data, 16_777_216).unwrap();
    let frames = full.samples.len() / full.channels as usize;
    for target in [frames - 1, 0, 211] {
        let result = indexed_seek::seek(
            source,
            d,
            Timestamp::new(target as i64),
            pool.take(),
            SeekContext {
                tx: &tx,
                state: &state,
                deadline: Instant::now() + Duration::from_secs(3),
            },
        )
        .await
        .unwrap()
        .unwrap();
        d = result.decoder;
        let SeekStatus::Applied {
            seeked,
            step,
            pcm,
            restart,
        } = result.status
        else {
            panic!("seek rejected")
        };
        assert_eq!(seeked.required_ts.get(), target as i64);
        assert_eq!(step.consumed, 0);
        assert_eq!(restart, !B);
        assert_ne!(step.progress, Progress::NeedSource);
        COLLECT.with(|v| v.borrow_mut().extend_from_slice(&pcm));
        drop(pcm);
        assert!(
            tx.capacity() == crate::streaming::STREAM_CHANNEL_CAPACITY,
            "seek helper published messages"
        );
        if step.progress != Progress::Finished {
            d = drain(source, d, &mut pool, &tx, &state, !restart).await;
        }
        let expected = resample(
            &full.samples[target * full.channels as usize..],
            full.sample_rate,
            target_rate,
            full.channels,
        );
        assert_pcm(&take_pcm(), &expected);
    }
    assert_eq!(trace.seeks.load(Ordering::Relaxed), 3);
    drop(d);
    assert_eq!(trace.drops.load(Ordering::Relaxed), 1);
    3
}
#[tokio::test(flavor = "current_thread")]
async fn real_http_and_file_seek_reuse_eof_owners_and_exact_dsp_suffixes() {
    let mut count = 0;
    for name in ["front", "tail", "reorder", "scaled", "edit"] {
        let name = format!("1-seek-{name}.m4a");
        let data = fixture(&name);
        for rate in [44100, 48000] {
            let owner = migo_io::scheduler::IoScheduler::new(0);
            let (mut s, _) = file(&name, &owner).await;
            count += matrix::<false, _>(&mut s, &owner, &data, rate).await;
            let (mut s, _) = file(&name, &owner).await;
            count += matrix::<true, _>(&mut s, &owner, &data, rate).await;
            let (_server, mut s) = http::<false>(
                &data,
                Config {
                    chunked: true,
                    ..Default::default()
                },
            )
            .await;
            count += matrix::<false, _>(&mut s, &owner, &data, rate).await;
            let (server, mut s) = http::<true>(
                &data,
                Config {
                    chunked: true,
                    no_tag: true,
                    ..Default::default()
                },
            )
            .await;
            count += matrix::<true, _>(&mut s, &owner, &data, rate).await;
            assert!(server.ranges.lock().unwrap().is_empty());
            assert_eq!(server.full_requests.load(Ordering::Relaxed), 1);
        }
    }
    assert_eq!(count, 120);
    println!("W09_SOURCE_SEEK_PCM {count} actual file/HTTP suffixes;error<1e-5");
}
#[tokio::test(flavor = "current_thread")]
async fn rejected_target_preserves_pending_source_and_recycles_the_same_loan() {
    take_pcm();
    let name = "1-seek-front.m4a";
    let data = fixture(name);
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let (mut s, reads) = file(name, &owner).await;
    let state = StreamingState::new();
    let (tx, _rx) = stream_channel();
    let mut pool = PcmPool::new();
    let mut d =
        decoder::<false>(&owner, data.len() as u64, 44100, Arc::new(Trace::default())).await;
    d = drain(&mut s, d, &mut pool, &tx, &state, false).await;
    take_pcm();
    let r = indexed_seek::seek(
        &mut s,
        d,
        Timestamp::new(211),
        pool.take(),
        SeekContext {
            tx: &tx,
            state: &state,
            deadline: Instant::now() + Duration::from_secs(3),
        },
    )
    .await
    .unwrap()
    .unwrap();
    d = r.decoder;
    let SeekStatus::Applied { pcm, .. } = r.status else {
        panic!("seek failed")
    };
    drop(pcm);
    let before = reads.load(Ordering::Acquire);
    for target in [-1, 9737, i64::MAX] {
        let loan = pool.take();
        let ptr = loan.as_ptr();
        let r = indexed_seek::seek(
            &mut s,
            d,
            Timestamp::new(target),
            loan,
            SeekContext {
                tx: &tx,
                state: &state,
                deadline: Instant::now() + Duration::from_secs(3),
            },
        )
        .await
        .unwrap()
        .unwrap();
        d = r.decoder;
        assert!(matches!(
            r.status,
            SeekStatus::Rejected(SeekErrorKind::OutOfRange)
        ));
        assert_eq!(reads.load(Ordering::Acquire), before);
        let loan = pool.take();
        assert_eq!(loan.as_ptr(), ptr);
        drop(loan);
    }
    d = drain(&mut s, d, &mut pool, &tx, &state, false).await;
    drop(d);
    assert_pcm(
        &take_pcm(),
        &crate::candidate::decode(&data, 16_777_216).unwrap().samples[422..],
    );
}
#[tokio::test(flavor = "current_thread")]
async fn uninitialized_cancelled_closed_and_expired_controls_do_not_touch_io() {
    let name = "1-seek-front.m4a";
    let owner = migo_io::scheduler::IoScheduler::new(0);
    for mode in 0..5 {
        let (mut source, reads) = file(name, &owner).await;
        let before = reads.load(Ordering::Acquire);
        let state = StreamingState::new();
        let (tx, rx) = stream_channel();
        let mut pool = PcmPool::new();
        let d = OffWorker::<Option<Observed<false>>>::new(None, owner.audio_cpu());
        if mode == 1 {
            state.cancel()
        };
        if mode == 2 {
            drop(rx)
        };
        let deadline = if mode == 3 {
            Instant::now() - Duration::from_millis(1)
        } else {
            Instant::now() + Duration::from_secs(3)
        };
        let mut pcm = pool.take();
        if mode == 4 {
            pcm.buffer_mut().push(1.0);
        }
        let result = indexed_seek::seek(
            &mut source,
            d,
            Timestamp::ZERO,
            pcm,
            SeekContext {
                tx: &tx,
                state: &state,
                deadline,
            },
        )
        .await;
        match mode {
            0 => {
                let r = result.unwrap().unwrap();
                assert!(matches!(
                    r.status,
                    SeekStatus::Rejected(SeekErrorKind::Unseekable)
                ));
                let (_, empty) = r.decoder.with(|v| v.is_none()).await.unwrap();
                assert!(empty)
            }
            1 | 2 => assert!(result.unwrap().is_none()),
            3 | 4 => assert!(result.is_err()),
            _ => unreachable!(),
        }
        assert_eq!(reads.load(Ordering::Acquire), before);
    }
}
#[tokio::test(flavor = "current_thread")]
async fn real_range_identity_failures_never_fall_back_to_an_unpinned_response() {
    let data = fixture("1-seek-front.m4a");
    for fault in [Fault::ChangedTag, Fault::IgnoredRange] {
        take_pcm();
        let (server, mut s) = http::<false>(
            &data,
            Config {
                media_fault: fault,
                ..Default::default()
            },
        )
        .await;
        let owner = migo_io::scheduler::IoScheduler::new(0);
        let state = StreamingState::new();
        let (tx, _rx) = stream_channel();
        let mut pool = PcmPool::new();
        let trace = Arc::new(Trace::default());
        let d = decoder::<false>(&owner, data.len() as u64, 44100, trace.clone()).await;
        let d = drain(&mut s, d, &mut pool, &tx, &state, false).await;
        take_pcm();
        let result = indexed_seek::seek(
            &mut s,
            d,
            Timestamp::ZERO,
            pool.take(),
            SeekContext {
                tx: &tx,
                state: &state,
                deadline: Instant::now() + Duration::from_secs(3),
            },
        )
        .await;
        assert!(result.is_err());
        assert_eq!(trace.drops.load(Ordering::Relaxed), 1);
        assert_eq!(server.full_requests.load(Ordering::Relaxed), 1);
        assert_eq!(tx.capacity(), crate::streaming::STREAM_CHANNEL_CAPACITY);
    }
}
#[tokio::test(flavor = "current_thread")]
async fn pending_media_seek_cancels_and_receiver_exit_retires_the_owner() {
    let data = fixture("1-seek-front.m4a");
    for close in [false, true] {
        take_pcm();
        let (server, mut s) = http::<false>(
            &data,
            Config {
                media_fault: Fault::HangHeaders,
                ..Default::default()
            },
        )
        .await;
        let owner = migo_io::scheduler::IoScheduler::new(0);
        let state = StreamingState::new();
        let (tx, rx) = stream_channel();
        let mut pool = PcmPool::new();
        let trace = Arc::new(Trace::default());
        let d = decoder::<false>(&owner, data.len() as u64, 44100, trace.clone()).await;
        let d = drain(&mut s, d, &mut pool, &tx, &state, false).await;
        take_pcm();
        let seek = indexed_seek::seek(
            &mut s,
            d,
            Timestamp::ZERO,
            pool.take(),
            SeekContext {
                tx: &tx,
                state: &state,
                deadline: Instant::now() + Duration::from_secs(3),
            },
        );
        tokio::pin!(seek);
        tokio::select! {r=&mut seek=>{let _=r;panic!("seek finished before media wait")},_=server.media_arrived.notified()=>{}}
        if close {
            drop(rx)
        } else {
            state.cancel()
        };
        let result = tokio::time::timeout(Duration::from_secs(1), &mut seek)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_none());
        assert!(state.is_cancelled());
        assert_eq!(trace.drops.load(Ordering::Relaxed), 1);
    }
}
struct Release(Option<std::sync::mpsc::Sender<()>>);
impl Drop for Release {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn cpu_deadline_returns_before_the_running_seek_and_skips_late_decode() {
    take_pcm();
    let name = "1-seek-front.m4a";
    let data = fixture(name);
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let (mut s, reads) = file(name, &owner).await;
    let state = StreamingState::new();
    let (tx, _rx) = stream_channel();
    let mut pool = PcmPool::new();
    let trace = Arc::new(Trace::default());
    let d = decoder::<false>(&owner, data.len() as u64, 44100, trace.clone()).await;
    let d = drain(&mut s, d, &mut pool, &tx, &state, false).await;
    take_pcm();
    let before = reads.load(Ordering::Acquire);
    let steps = trace.steps.load(Ordering::Relaxed);
    let (send, recv) = std::sync::mpsc::channel();
    let release = Release(Some(send));
    *trace.block_seek.lock().unwrap() = Some(recv);
    let seek = indexed_seek::seek(
        &mut s,
        d,
        Timestamp::ZERO,
        pool.take(),
        SeekContext {
            tx: &tx,
            state: &state,
            deadline: Instant::now() + Duration::from_millis(100),
        },
    );
    tokio::pin!(seek);
    tokio::select! {r=&mut seek=>{let _=r;panic!("seek finished before entering CPU")},_=trace.seek_entered.notified()=>{}}
    let r = tokio::time::timeout(Duration::from_millis(300), &mut seek).await;
    assert!(r.is_ok(), "seek caller waited for the blocked CPU job");
    assert!(r.unwrap().is_err());
    drop(release);
    tokio::time::timeout(Duration::from_secs(1), trace.dropped.notified())
        .await
        .unwrap();
    assert_eq!(reads.load(Ordering::Acquire), before);
    assert_eq!(trace.steps.load(Ordering::Relaxed), steps);
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_seek_steps_and_fatal_index_errors_never_enter_source_io() {
    let name = "1-seek-tail.m4a";
    let data = fixture(name);
    for mode in 0..3 {
        take_pcm();
        let owner = migo_io::scheduler::IoScheduler::new(0);
        let (mut s, reads) = file(name, &owner).await;
        let state = StreamingState::new();
        let (tx, _rx) = stream_channel();
        let mut pool = PcmPool::new();
        let trace = Arc::new(Trace::default());
        let d = decoder::<false>(&owner, data.len() as u64, 44100, trace.clone()).await;
        let d = drain(&mut s, d, &mut pool, &tx, &state, false).await;
        take_pcm();
        let before = reads.load(Ordering::Acquire);
        match mode {
            0 => trace.bad_consumed.store(true, Ordering::Relaxed),
            1 => {
                let mut at = 0;
                let media = loop {
                    let span = AtomSpan::read(&data[at..], at as u64, Some(data.len() as u64))
                        .unwrap()
                        .unwrap();
                    if span.kind == symphonia_format_isomp4::streaming::AtomKind::MediaData {
                        break span;
                    }
                    at = span.end.unwrap() as usize;
                };
                *trace.request_override.lock().unwrap() = Some(M4aSourceRequest::Index(
                    symphonia_format_isomp4::streaming::IndexRequest {
                        media,
                        next_atom_offset: media.end,
                    },
                ));
            }
            2 => trace.fail_seek.store(true, Ordering::Relaxed),
            _ => unreachable!(),
        }
        let r = indexed_seek::seek(
            &mut s,
            d,
            Timestamp::new(211),
            pool.take(),
            SeekContext {
                tx: &tx,
                state: &state,
                deadline: Instant::now() + Duration::from_secs(3),
            },
        )
        .await;
        assert!(r.is_err());
        assert_eq!(reads.load(Ordering::Acquire), before);
        assert_eq!(trace.drops.load(Ordering::Relaxed), 1);
    }
}
struct Paused<S> {
    inner: S,
    pause: bool,
    lie: bool,
    deadline: Arc<Mutex<Option<Instant>>>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}
impl<const B: bool, S: SeekSource<Observed<B>>> Source<Observed<B>> for Paused<S> {
    type Request = M4aSourceRequest;
    fn pending(value: &Observed<B>) -> EngineResult<M4aSourceRequest> {
        S::pending(value)
    }
    async fn read(
        &mut self,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
    ) -> EngineResult<SourceRead<impl AsRef<[u8]> + Send + 'static>> {
        self.inner.read(tx, state).await
    }
    async fn resume(
        &mut self,
        request: Self::Request,
        decoder: OffWorker<Option<Observed<B>>>,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
    ) -> EngineResult<Option<crate::streaming_transport::SourceResume<Observed<B>>>> {
        self.inner.resume(request, decoder, tx, state).await
    }
}
impl<const B: bool, S: SeekSource<Observed<B>>> SeekSource<Observed<B>> for Paused<S> {
    async fn resume_before(
        &mut self,
        request: M4aSourceRequest,
        decoder: OffWorker<Option<Observed<B>>>,
        context: SeekContext<'_, '_>,
    ) -> EngineResult<Option<crate::streaming_transport::SourceResume<Observed<B>>>> {
        *self.deadline.lock().unwrap() = Some(context.deadline);
        self.entered.notify_one();
        if self.pause {
            self.release.notified().await;
        }
        let mut result = self.inner.resume_before(request, decoder, context).await?;
        if self.lie {
            if let Some(result) = &mut result {
                result.restart = false;
            }
        }
        Ok(result)
    }
}
#[tokio::test(flavor = "current_thread")]
async fn source_deadline_is_not_renewed_and_a_real_reposition_must_replace_the_body() {
    let name = "1-seek-front.m4a";
    let data = fixture(name);
    for pause in [true, false] {
        take_pcm();
        let owner = migo_io::scheduler::IoScheduler::new(0);
        let (s, _) = file(name, &owner).await;
        let state = StreamingState::new();
        let (tx, _rx) = stream_channel();
        let mut pool = PcmPool::new();
        let trace = Arc::new(Trace::default());
        let d = decoder::<false>(&owner, data.len() as u64, 44100, trace.clone()).await;
        let seen = Arc::new(Mutex::new(None));
        let entered = Arc::new(Notify::new());
        let mut s = Paused {
            inner: s,
            pause,
            lie: !pause,
            deadline: seen.clone(),
            entered: entered.clone(),
            release: Arc::new(Notify::new()),
        };
        let d = drain(&mut s, d, &mut pool, &tx, &state, false).await;
        take_pcm();
        let deadline = Instant::now() + Duration::from_millis(100);
        let operation = indexed_seek::seek(
            &mut s,
            d,
            Timestamp::new(211),
            pool.take(),
            SeekContext {
                tx: &tx,
                state: &state,
                deadline,
            },
        );
        tokio::pin!(operation);
        let r = tokio::time::timeout(Duration::from_millis(300), &mut operation).await;
        assert!(r.is_ok(), "source seek ignored the absolute outer deadline");
        assert!(r.unwrap().is_err());
        assert_eq!(*seen.lock().unwrap(), Some(deadline));
        assert_eq!(trace.drops.load(Ordering::Relaxed), 1);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cached_seek_adds_no_caller_allocation_beyond_one_existing_cpu_handoff() {
    take_pcm();
    let name = "1-seek-front.m4a";
    let data = fixture(name);
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let (mut s, reads) = file(name, &owner).await;
    let state = StreamingState::new();
    let (tx, _rx) = stream_channel();
    let mut pool = PcmPool::new();
    let mut d = decoder::<true>(&owner, data.len() as u64, 48000, Arc::new(Trace::default())).await;
    d = drain(&mut s, d, &mut pool, &tx, &state, false).await;
    take_pcm();
    let before_io = reads.load(Ordering::Acquire);
    let mut before = 0;
    for i in 0..40 {
        if i == 8 {
            before = migo_alloc_probe::thread_counts().allocation_events();
        }
        let r = indexed_seek::seek(
            &mut s,
            d,
            Timestamp::new(211),
            pool.take(),
            SeekContext {
                tx: &tx,
                state: &state,
                deadline: Instant::now() + Duration::from_secs(3),
            },
        )
        .await
        .unwrap()
        .unwrap();
        d = r.decoder;
        let SeekStatus::Applied { pcm, restart, .. } = r.status else {
            panic!("cached seek rejected")
        };
        assert!(!restart);
        drop(pcm);
    }
    let events = migo_alloc_probe::thread_counts().allocation_events() - before;
    assert_eq!(
        events, 32,
        "cached seek should use only the existing one-allocation CPU handoff;CPU/native allocations are outside this caller counter"
    );
    assert_eq!(reads.load(Ordering::Acquire), before_io);
    drop(d);
    println!("W09_SOURCE_SEEK_ALLOC 32 warmed RPCs/32 caller allocations;0source reads");
}

#[tokio::test(flavor = "current_thread")]
async fn seek_future_fits_the_default_stack_without_boxing_or_stack_overrides() {
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let (mut s, _) = file("1-seek-front.m4a", &owner).await;
    let state = StreamingState::new();
    let (tx, _rx) = stream_channel();
    let mut pool = PcmPool::new();
    let d = OffWorker::<Option<Observed<false>>>::new(None, owner.audio_cpu());
    let future = indexed_seek::seek(
        &mut s,
        d,
        Timestamp::ZERO,
        pool.take(),
        SeekContext {
            tx: &tx,
            state: &state,
            deadline: Instant::now() + Duration::from_secs(3),
        },
    );
    let bytes = std::mem::size_of_val(&future);
    assert!(
        bytes <= 320 * 1024,
        "seek future retains too many full decoder/IO futures: {bytes} bytes"
    );
    drop(future);
    println!("W09_SOURCE_SEEK_FUTURE {bytes} bytes;budget327680;defaultstack");
}
