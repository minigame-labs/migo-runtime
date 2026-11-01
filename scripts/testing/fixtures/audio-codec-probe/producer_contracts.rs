//! The same producer must retain owners, pending PCM and EOF across controls.
use super::*;
use crate::{
    candidate,
    decoder_seek_contracts::{assert_pcm, resample},
    isomp4_file::{FileReader, IndexedFile},
    isomp4_source_contracts::{Config, client, fixture, start_server},
    isomp4_transport::IndexedHttp,
    push_decoder::{M4a, M4aSourceDecoder, M4aSourceRequest},
    resampled_streaming::Batched,
    streaming::{StreamMsg, stream_channel},
    streaming_transport::{
        self as transport, HttpSource, InterruptibleSource, Progress, Source, SourceRead,
        SourceResume, Step,
    },
};
use crate::{
    inner_audio::InnerAudioPlayer,
    streaming::{STREAM_CHANNEL_CAPACITY, StreamRevision},
};
use std::{
    future::Future,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};
use symphonia::core::{
    errors::{Error, Result as FormatResult},
    formats::SeekedTo,
};
use symphonia_format_isomp4::streaming::{AtomSpan, AudioIndex};
use tokio::{sync::Notify, task::JoinHandle};

type D<const B: bool> = Batched<1, M4a<B>>;
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}
struct Cancel(Arc<StreamingState>);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
async fn file_source(owner: &migo_io::scheduler::IoScheduler, name: &str) -> IndexedFile {
    let path = std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join(name);
    IndexedFile::open(std::fs::File::open(path).unwrap(), owner)
        .await
        .unwrap()
}
async fn http_source<const B: bool>(
    data: Vec<u8>,
    config: Config,
) -> (crate::isomp4_source_contracts::Server, IndexedHttp) {
    let server = start_server(data, config).await;
    let client = client();
    let response = <IndexedHttp as HttpSource<D<B>>>::request(client.get(
        crate::network::gate::GateKind::AudioStream,
        server.url.clone(),
    ))
    .send()
    .await
    .unwrap();
    let source = <IndexedHttp as HttpSource<D<B>>>::new(client, response).unwrap();
    (server, source)
}
/// Notify only after the second actual media read has returned Pending. Byte
/// counters alone cannot distinguish a CPU step from a suspended body read.
struct ObservedRead<S> {
    source: S,
    reads: usize,
    waiting: Arc<Notify>,
}
impl<S: InterruptibleSource> InterruptibleSource for ObservedRead<S> {
    fn resume_after_read_interruption(&mut self) -> EngineResult<()> {
        self.source.resume_after_read_interruption()
    }
}
impl<T: Decoder + M4aSeekDecoder, S: SeekSource<T>> Source<T> for ObservedRead<S> {
    type Request = M4aSourceRequest;
    fn pending(decoder: &T) -> EngineResult<Self::Request> {
        S::pending(decoder)
    }
    async fn read(
        &mut self,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
    ) -> EngineResult<SourceRead<impl AsRef<[u8]> + Send + 'static>> {
        self.reads += 1;
        let notify = self.reads >= 2;
        let waiting = &self.waiting;
        let operation = self.source.read(tx, state);
        tokio::pin!(operation);
        std::future::poll_fn(|cx| {
            let result = operation.as_mut().poll(cx);
            if notify && result.is_pending() {
                waiting.notify_one();
            }
            result
        })
        .await
    }
    async fn resume(
        &mut self,
        request: Self::Request,
        decoder: OffWorker<Option<T>>,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
    ) -> EngineResult<Option<SourceResume<T>>> {
        self.source.resume(request, decoder, tx, state).await
    }
}
impl<T: Decoder + M4aSeekDecoder, S: SeekSource<T>> SeekSource<T> for ObservedRead<S> {
    async fn resume_before(
        &mut self,
        request: Self::Request,
        decoder: OffWorker<Option<T>>,
        context: SeekContext<'_, '_>,
    ) -> EngineResult<Option<SourceResume<T>>> {
        self.source.resume_before(request, decoder, context).await
    }
}
struct ActorTask {
    handle: JoinHandle<EngineResult<()>>,
    _owner: migo_io::scheduler::IoScheduler,
}
impl ActorTask {
    fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }
}
impl Future for ActorTask {
    type Output = Result<EngineResult<()>, tokio::task::JoinError>;
    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        std::pin::Pin::new(&mut self.get_mut().handle).poll(cx)
    }
}
fn spawn<const B: bool, S: SeekSource<D<B>> + 'static>(
    owner: migo_io::scheduler::IoScheduler,
    source: S,
    len: u64,
    rate: u32,
    tx: mpsc::Sender<StreamMsg>,
    state: Arc<StreamingState>,
    controls: Controls,
) -> ActorTask {
    // The caller retains its host, as AudioService does. Spawn the owned
    // producer directly; another async owner wrapper needlessly copies it.
    let handle = tokio::spawn(transport::run_source_controlled(
        owner.audio_cpu(),
        source,
        Some(len),
        tx,
        state,
        move |len| D::<B>::with_object_len(rate, len),
        controls,
    ));
    ActorTask {
        handle,
        _owner: owner,
    }
}
/// Count completed nonempty PCM steps, including the frame blocked outside
/// the four-slot queue. Ready occupies the first slot, so four PCM steps prove
/// that a real fifth message is waiting without relying on scheduler sleeps.
struct Counted<T> {
    inner: T,
    pcm_steps: Arc<AtomicUsize>,
}
impl<T: Decoder> Decoder for Counted<T> {
    fn step(
        &mut self,
        input: &[u8],
        eof: bool,
        pcm: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<Step> {
        let result = self.inner.step(input, eof, pcm, cancel)?;
        if !pcm.is_empty() {
            self.pcm_steps.fetch_add(1, Ordering::Release);
        }
        Ok(result)
    }
}
impl<T: M4aSourceDecoder> M4aSourceDecoder for Counted<T> {
    fn source_request(&self) -> Option<M4aSourceRequest> {
        self.inner.source_request()
    }
    fn install_index_with_length(
        &mut self,
        index: AudioIndex,
        len: Option<u64>,
    ) -> Result<(), String> {
        self.inner.install_index_with_length(index, len)
    }
    fn resume_at(&mut self, span: AtomSpan, offset: u64) -> Result<(), String> {
        self.inner.resume_at(span, offset)
    }
}
impl<T: M4aSeekDecoder> M4aSeekDecoder for Counted<T> {
    fn seek(&mut self, target: Timestamp) -> FormatResult<SeekedTo> {
        self.inner.seek(target)
    }
}
fn spawn_counted<const B: bool, S: SeekSource<Counted<D<B>>> + 'static>(
    owner: migo_io::scheduler::IoScheduler,
    source: S,
    len: u64,
    rate: u32,
    endpoints: (mpsc::Sender<StreamMsg>, Arc<StreamingState>, Controls),
    pcm_steps: Arc<AtomicUsize>,
) -> ActorTask {
    let (tx, state, controls) = endpoints;
    let handle = tokio::spawn(transport::run_source_controlled(
        owner.audio_cpu(),
        source,
        Some(len),
        tx,
        state,
        move |len| Counted {
            inner: D::<B>::with_object_len(rate, len),
            pcm_steps,
        },
        controls,
    ));
    ActorTask {
        handle,
        _owner: owner,
    }
}
async fn pending_pcm(pcm_steps: &AtomicUsize) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while pcm_steps.load(Ordering::Acquire) < crate::streaming::STREAM_CHANNEL_CAPACITY {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "producer held only {} nonempty PCM steps",
            pcm_steps.load(Ordering::Acquire)
        )
    });
}
async fn receive(rx: &mut mpsc::Receiver<StreamMsg>, revision: StreamRevision) -> Vec<f32> {
    let mut pcm = Vec::new();
    let mut ready = 0;
    loop {
        let message = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .expect("source closed before Done");
        if message.revision() != revision {
            continue;
        }
        match message {
            StreamMsg::Ready { .. } => {
                ready += 1;
                assert_eq!(ready, 1);
            }
            StreamMsg::Samples { pcm: chunk, .. } => {
                assert_eq!(ready, 1);
                pcm.extend_from_slice(&chunk);
            }
            StreamMsg::Done { .. } => {
                assert_eq!(ready, 1);
                return pcm;
            }
            StreamMsg::Error { message, .. } => panic!("producer error: {message}"),
        }
    }
}
async fn prepared(reply: oneshot::Receiver<Reply>) -> Prepared {
    match tokio::time::timeout(Duration::from_secs(3), reply)
        .await
        .unwrap()
        .unwrap()
    {
        Reply::Prepared(value) => value,
        _ => panic!("valid target was not prepared"),
    }
}
fn acknowledge(value: Prepared, state: &Arc<StreamingState>, target: i64) -> StreamRevision {
    assert!(Arc::ptr_eq(&value.source, state));
    assert_eq!(value.target, target);
    let revision = value.revision;
    value.accept.send(()).unwrap();
    revision
}
async fn saturated(tx: &mpsc::Sender<StreamMsg>) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while tx.capacity() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
async fn stop(task: ActorTask, state: &StreamingState) {
    state.cancel();
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
fn future_bytes<F: Future>(_: impl FnOnce() -> F) -> usize {
    std::mem::size_of::<F>()
}

#[tokio::test]
async fn producer_future_fits_default_stack_without_boxing_or_stack_overrides() {
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let source = file_source(&owner, "1-seek-front.m4a").await;
    let state = StreamingState::new_reusable();
    let (tx, _rx) = stream_channel();
    let (_requests, controls) = channel();
    let bytes = future_bytes(|| {
        transport::run_source_controlled(
            owner.audio_cpu(),
            source,
            None,
            tx.clone(),
            state.clone(),
            |len| D::<false>::with_object_len(48000, len),
            controls,
        )
    });
    println!("W09_PRODUCER_FUTURE {bytes} bytes;defaultstack;unboxed");
    assert!(
        bytes <= 320 * 1024,
        "producer retains too many full decoder/IO futures: {bytes}"
    );
}

async fn eof_matrix<const B: bool, S: SeekSource<D<B>> + 'static>(
    owner: migo_io::scheduler::IoScheduler,
    source: S,
    data: &[u8],
    rate: u32,
) {
    let full = candidate::decode(data, 16 * 1024 * 1024).unwrap();
    let (tx, mut rx) = stream_channel();
    let (state_tx, controls) = channel();
    let state = StreamingState::new_reusable();
    let _cancel = Cancel(state.clone());
    let task = spawn::<B, _>(
        owner,
        source,
        data.len() as u64,
        rate,
        tx,
        state.clone(),
        controls,
    );
    assert_pcm(
        &receive(&mut rx, StreamRevision::INITIAL).await,
        &resample(&full.samples, full.sample_rate, rate, full.channels),
    );
    assert!(!task.is_finished(), "EOF dropped the source actor");
    let mut last = StreamRevision::INITIAL;
    for target in [1000, 0, full.samples.len() / full.channels as usize / 2] {
        let value =
            prepared(request(&state_tx, Timestamp::new(target as i64), deadline()).unwrap()).await;
        assert_eq!(value.revision, last.checked_next().unwrap());
        assert!(
            rx.try_recv().is_err(),
            "new traversal published before acceptance"
        );
        let revision = acknowledge(value, &state, target as i64);
        let expected = resample(
            &full.samples[target * full.channels as usize..],
            full.sample_rate,
            rate,
            full.channels,
        );
        assert_pcm(&receive(&mut rx, revision).await, &expected);
        last = revision;
        assert!(!task.is_finished());
    }
    drop(state_tx);
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
#[tokio::test]
async fn file_and_http_keep_one_actor_for_eof_seek_restart_and_exact_pcm_suffixes() {
    for name in ["1-seek-front.m4a", "1-seek-tail.m4a"] {
        let data = fixture(name);
        for rate in [44100, 48000] {
            let owner = migo_io::scheduler::IoScheduler::new(0);
            let source = file_source(&owner, name).await;
            eof_matrix::<false, _>(owner, source, &data, rate).await;
            let owner = migo_io::scheduler::IoScheduler::new(0);
            let source = file_source(&owner, name).await;
            eof_matrix::<true, _>(owner, source, &data, rate).await;
            let (_server, source) = http_source::<false>(data.clone(), Config::default()).await;
            eof_matrix::<false, _>(migo_io::scheduler::IoScheduler::new(0), source, &data, rate)
                .await;
            let (_server, source) = http_source::<true>(data.clone(), Config::default()).await;
            eof_matrix::<true, _>(migo_io::scheduler::IoScheduler::new(0), source, &data, rate)
                .await;
        }
    }
    println!(
        "W09_PRODUCER_PCM 48 actual file/HTTP traversal suffix comparisons;fresh-resampler reference only"
    );
}
#[tokio::test]
async fn full_output_queue_accepts_seek_without_publishing_before_consumer_ack() {
    let name = "2-seek-front.m4a";
    let data = fixture(name);
    let full = candidate::decode(&data, 16 * 1024 * 1024).unwrap();
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let source = file_source(&owner, name).await;
    let (tx, mut rx) = stream_channel();
    let monitor = tx.clone();
    let (state_tx, controls) = channel();
    let state = StreamingState::new_reusable();
    let _cancel = Cancel(state.clone());
    let pcm_steps = Arc::new(AtomicUsize::new(0));
    let task = spawn_counted::<false, _>(
        owner,
        source,
        data.len() as u64,
        full.sample_rate,
        (tx, state.clone(), controls),
        pcm_steps.clone(),
    );
    saturated(&monitor).await;
    pending_pcm(&pcm_steps).await;
    let value = prepared(request(&state_tx, Timestamp::new(1000), deadline()).unwrap()).await;
    while let Ok(message) = rx.try_recv() {
        assert_eq!(message.revision(), StreamRevision::INITIAL);
    }
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    assert!(rx.try_recv().is_err());
    let revision = acknowledge(value, &state, 1000);
    assert_pcm(
        &receive(&mut rx, revision).await,
        &full.samples[1000 * full.channels as usize..],
    );
    stop(task, &state).await;
}
#[tokio::test]
async fn rejected_seek_under_backpressure_keeps_the_exact_old_pending_pcm() {
    let name = "2-seek-front.m4a";
    let data = fixture(name);
    let full = candidate::decode(&data, 16 * 1024 * 1024).unwrap();
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let source = file_source(&owner, name).await;
    let (tx, mut rx) = stream_channel();
    let monitor = tx.clone();
    let (state_tx, controls) = channel();
    let state = StreamingState::new_reusable();
    let _cancel = Cancel(state.clone());
    let pcm_steps = Arc::new(AtomicUsize::new(0));
    let task = spawn_counted::<true, _>(
        owner,
        source,
        data.len() as u64,
        full.sample_rate,
        (tx, state.clone(), controls),
        pcm_steps.clone(),
    );
    saturated(&monitor).await;
    pending_pcm(&pcm_steps).await;
    let reply = request(&state_tx, Timestamp::new(i64::MAX), deadline()).unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(3), reply)
            .await
            .unwrap()
            .unwrap(),
        Reply::Rejected(_)
    ));
    assert_pcm(
        &receive(&mut rx, StreamRevision::INITIAL).await,
        &full.samples,
    );
    assert!(!task.is_finished());
    stop(task, &state).await;
}
#[tokio::test]
async fn closing_controls_during_a_pending_http_read_keeps_the_original_body() {
    let data = fixture("1-seek-front.m4a");
    let full = candidate::decode(&data, 16 * 1024 * 1024).unwrap();
    let (server, source) = http_source::<true>(
        data.clone(),
        Config {
            hold: Some(128),
            ..Config::default()
        },
    )
    .await;
    let waiting = Arc::new(Notify::new());
    let source = ObservedRead {
        source,
        reads: 0,
        waiting: waiting.clone(),
    };
    let (tx, mut rx) = stream_channel();
    let (state_tx, controls) = channel();
    let state = StreamingState::new_reusable();
    let _cancel = Cancel(state.clone());
    let task = spawn::<true, _>(
        migo_io::scheduler::IoScheduler::new(0),
        source,
        data.len() as u64,
        full.sample_rate,
        tx,
        state.clone(),
        controls,
    );
    tokio::time::timeout(Duration::from_secs(3), waiting.notified())
        .await
        .expect("second actual HTTP read never became pending");
    drop(state_tx);
    server.release.notify_one();
    assert_pcm(
        &receive(&mut rx, StreamRevision::INITIAL).await,
        &full.samples,
    );
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(server.full_requests.load(Ordering::Acquire), 1);
}
#[tokio::test]
async fn expired_and_abandoned_unstarted_requests_preserve_eof_and_revision() {
    let data = fixture("1-seek-front.m4a");
    let full = candidate::decode(&data, 16 * 1024 * 1024).unwrap();
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let source = file_source(&owner, "1-seek-front.m4a").await;
    let (tx, mut rx) = stream_channel();
    let (state_tx, controls) = channel();
    drop(request(&state_tx, Timestamp::ZERO, deadline()).unwrap());
    let state = StreamingState::new_reusable();
    let _cancel = Cancel(state.clone());
    let task = spawn::<true, _>(
        owner,
        source,
        data.len() as u64,
        full.sample_rate,
        tx,
        state.clone(),
        controls,
    );
    receive(&mut rx, StreamRevision::INITIAL).await;
    let reply = request(
        &state_tx,
        Timestamp::ZERO,
        Instant::now() - Duration::from_secs(1),
    )
    .unwrap();
    assert!(matches!(
        reply.await.unwrap(),
        Reply::NotStarted(ErrorCode::Timeout)
    ));
    assert!(state.download_complete.load(Ordering::Acquire));
    assert!(rx.try_recv().is_err());
    let value = prepared(request(&state_tx, Timestamp::ZERO, deadline()).unwrap()).await;
    assert_eq!(
        value.revision,
        StreamRevision::INITIAL.checked_next().unwrap()
    );
    let rev = acknowledge(value, &state, 0);
    assert_pcm(&receive(&mut rx, rev).await, &full.samples);
    stop(task, &state).await;
}
#[tokio::test]
async fn abandoned_prepared_acceptance_cancels_the_source_instead_of_reusing_mutated_decoder() {
    let data = fixture("1-seek-front.m4a");
    let full = candidate::decode(&data, 16 * 1024 * 1024).unwrap();
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let source = file_source(&owner, "1-seek-front.m4a").await;
    let (tx, mut rx) = stream_channel();
    let (state_tx, controls) = channel();
    let state = StreamingState::new_reusable();
    let _cancel = Cancel(state.clone());
    let task = spawn::<true, _>(
        owner,
        source,
        data.len() as u64,
        full.sample_rate,
        tx,
        state.clone(),
        controls,
    );
    receive(&mut rx, StreamRevision::INITIAL).await;
    drop(prepared(request(&state_tx, Timestamp::ZERO, deadline()).unwrap()).await);
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
    assert!(
        state.is_cancelled(),
        "abandoned accepted source was revived"
    );
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(rx.try_recv().is_err());
}
#[test]
fn control_admission_is_bounded_and_closed_senders_fail_explicitly() {
    let (tx, controls) = channel();
    let mut replies = Vec::new();
    for _ in 0..STREAM_CHANNEL_CAPACITY {
        replies.push(request(&tx, Timestamp::ZERO, deadline()).unwrap());
    }
    assert_eq!(
        request(&tx, Timestamp::ZERO, deadline())
            .err()
            .unwrap()
            .code,
        ErrorCode::InputSaturated
    );
    drop(controls);
    assert_eq!(
        request(&tx, Timestamp::ZERO, deadline())
            .err()
            .unwrap()
            .code,
        ErrorCode::IoError
    );
}

#[derive(Default)]
struct Trace {
    created: AtomicUsize,
    drops: AtomicUsize,
    steps: AtomicUsize,
    seeks: AtomicUsize,
    reads: AtomicUsize,
    entered: Notify,
    eof: Notify,
    dropped: Notify,
    block: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}
struct Tiny {
    trace: Arc<Trace>,
}
impl Drop for Tiny {
    fn drop(&mut self) {
        self.trace.drops.fetch_add(1, Ordering::Relaxed);
        self.trace.dropped.notify_one();
    }
}
impl Decoder for Tiny {
    fn step(
        &mut self,
        input: &[u8],
        eof: bool,
        pcm: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<Step> {
        self.trace.steps.fetch_add(1, Ordering::Relaxed);
        self.trace.entered.notify_one();
        if let Some(block) = self.trace.block.lock().unwrap().take() {
            block.recv_timeout(Duration::from_secs(3)).unwrap();
        }
        if cancel.load(Ordering::Acquire) {
            return Ok(Step {
                consumed: 0,
                progress: Progress::Cancelled,
                sample_rate: 8000,
                channels: 1,
            });
        }
        let consumed = usize::from(!input.is_empty());
        if consumed != 0 {
            pcm.push(input[0] as f32);
        }
        if eof {
            self.trace.eof.notify_one();
        }
        Ok(Step {
            consumed,
            progress: if eof {
                Progress::Finished
            } else {
                Progress::NeedInput
            },
            sample_rate: 8000,
            channels: 1,
        })
    }
}
impl M4aSourceDecoder for Tiny {
    fn source_request(&self) -> Option<M4aSourceRequest> {
        None
    }
    fn install_index_with_length(
        &mut self,
        _index: AudioIndex,
        _len: Option<u64>,
    ) -> Result<(), String> {
        Err("unexpected index".into())
    }
    fn resume_at(&mut self, _span: AtomSpan, _offset: u64) -> Result<(), String> {
        Err("unexpected resume".into())
    }
}
impl M4aSeekDecoder for Tiny {
    fn seek(&mut self, _target: Timestamp) -> FormatResult<SeekedTo> {
        self.trace.seeks.fetch_add(1, Ordering::Relaxed);
        Err(Error::SeekError(SeekErrorKind::OutOfRange))
    }
}
struct MemorySource {
    trace: Arc<Trace>,
    sent: bool,
}
impl InterruptibleSource for MemorySource {
    fn resume_after_read_interruption(&mut self) -> EngineResult<()> {
        Ok(())
    }
}
impl Source<Tiny> for MemorySource {
    type Request = M4aSourceRequest;
    fn pending(_d: &Tiny) -> EngineResult<Self::Request> {
        Err(invalid("unexpected source request"))
    }
    async fn read(
        &mut self,
        _tx: &mpsc::Sender<StreamMsg>,
        _state: &Arc<StreamingState>,
    ) -> EngineResult<SourceRead<impl AsRef<[u8]> + Send + 'static>> {
        self.trace.reads.fetch_add(1, Ordering::Relaxed);
        if self.sent {
            Ok(SourceRead::End)
        } else {
            self.sent = true;
            Ok(SourceRead::Data(vec![1, 2, 3]))
        }
    }
    async fn resume(
        &mut self,
        _request: Self::Request,
        _decoder: OffWorker<Option<Tiny>>,
        _tx: &mpsc::Sender<StreamMsg>,
        _state: &Arc<StreamingState>,
    ) -> EngineResult<Option<SourceResume<Tiny>>> {
        Err(invalid("unexpected source resume"))
    }
}
impl SeekSource<Tiny> for MemorySource {
    async fn resume_before(
        &mut self,
        _request: M4aSourceRequest,
        _decoder: OffWorker<Option<Tiny>>,
        _context: SeekContext<'_, '_>,
    ) -> EngineResult<Option<SourceResume<Tiny>>> {
        Err(invalid("unexpected source resume"))
    }
}
fn spawn_tiny(
    trace: Arc<Trace>,
    tx: mpsc::Sender<StreamMsg>,
    state: Arc<StreamingState>,
    controls: Controls,
) -> ActorTask {
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let source = MemorySource {
        trace: trace.clone(),
        sent: false,
    };
    let handle = tokio::spawn(transport::run_source_controlled(
        owner.audio_cpu(),
        source,
        Some(3),
        tx,
        state,
        move |_| {
            trace.created.fetch_add(1, Ordering::Relaxed);
            Tiny { trace }
        },
        controls,
    ));
    ActorTask {
        handle,
        _owner: owner,
    }
}

#[tokio::test]
async fn soft_control_waits_for_cpu_owner_and_preserves_its_unpublished_pcm() {
    let trace = Arc::new(Trace::default());
    let (release, block) = std::sync::mpsc::channel();
    *trace.block.lock().unwrap() = Some(block);
    let (tx, mut rx) = stream_channel();
    let (commands, controls) = channel();
    let state = StreamingState::new_reusable();
    let _cancel = Cancel(state.clone());
    let task = spawn_tiny(trace.clone(), tx, state.clone(), controls);
    tokio::time::timeout(Duration::from_secs(2), trace.entered.notified())
        .await
        .unwrap();
    let mut reply = request(&commands, Timestamp::new(9), deadline()).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(5), &mut reply)
            .await
            .is_err()
    );
    assert_eq!(trace.seeks.load(Ordering::Acquire), 0);
    assert_eq!(trace.drops.load(Ordering::Acquire), 0);
    release.send(()).unwrap();
    assert!(matches!(
        reply.await.unwrap(),
        Reply::Rejected(ErrorCode::InvalidArgument)
    ));
    assert_pcm(
        &receive(&mut rx, StreamRevision::INITIAL).await,
        &[1., 2., 3.],
    );
    drop(commands);
    task.await.unwrap().unwrap();
    assert_eq!(trace.created.load(Ordering::Acquire), 1);
    assert_eq!(trace.drops.load(Ordering::Acquire), 1);
}
#[tokio::test]
async fn rejected_control_keeps_a_blocked_done_and_eof_wait_has_no_read_or_cpu_polling() {
    let trace = Arc::new(Trace::default());
    let (tx, mut rx) = stream_channel();
    let monitor = tx.clone();
    let (commands, controls) = channel();
    let state = StreamingState::new_reusable();
    let _cancel = Cancel(state.clone());
    let task = spawn_tiny(trace.clone(), tx, state.clone(), controls);
    tokio::time::timeout(Duration::from_secs(2), trace.eof.notified())
        .await
        .unwrap();
    saturated(&monitor).await;
    let reply = request(&commands, Timestamp::new(9), deadline()).unwrap();
    assert!(matches!(reply.await.unwrap(), Reply::Rejected(_)));
    assert_pcm(
        &receive(&mut rx, StreamRevision::INITIAL).await,
        &[1., 2., 3.],
    );
    let counts = (
        trace.reads.load(Ordering::Acquire),
        trace.steps.load(Ordering::Acquire),
    );
    for _ in 0..128 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        counts,
        (
            trace.reads.load(Ordering::Acquire),
            trace.steps.load(Ordering::Acquire)
        )
    );
    assert!(rx.try_recv().is_err());
    assert!(!task.is_finished());
    drop(commands);
    task.await.unwrap().unwrap();
    assert_eq!(trace.drops.load(Ordering::Acquire), 1);
}
#[tokio::test]
async fn receiver_exit_retires_a_backpressured_actor_and_pending_loans() {
    let trace = Arc::new(Trace::default());
    let (tx, rx) = stream_channel();
    let monitor = tx.clone();
    let (_commands, controls) = channel();
    let state = StreamingState::new_reusable();
    let task = spawn_tiny(trace.clone(), tx, state.clone(), controls);
    saturated(&monitor).await;
    drop(rx);
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(state.is_cancelled());
    assert_eq!(trace.drops.load(Ordering::Acquire), 1);
}
#[tokio::test]
async fn cancelling_a_running_cpu_step_returns_without_adopting_its_late_owner() {
    let trace = Arc::new(Trace::default());
    let (release, block) = std::sync::mpsc::channel();
    *trace.block.lock().unwrap() = Some(block);
    let (tx, _rx) = stream_channel();
    let (_commands, controls) = channel();
    let state = StreamingState::new_reusable();
    let task = spawn_tiny(trace.clone(), tx, state.clone(), controls);
    tokio::time::timeout(Duration::from_secs(2), trace.entered.notified())
        .await
        .unwrap();
    state.cancel();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(trace.drops.load(Ordering::Acquire), 0);
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), trace.dropped.notified())
        .await
        .unwrap();
    assert_eq!(trace.drops.load(Ordering::Acquire), 1);
}

struct FailReads<S> {
    source: S,
    fail: Arc<AtomicBool>,
}
impl<S: InterruptibleSource> InterruptibleSource for FailReads<S> {
    fn resume_after_read_interruption(&mut self) -> EngineResult<()> {
        self.source.resume_after_read_interruption()
    }
}
impl<T: Decoder + M4aSeekDecoder, S: SeekSource<T>> Source<T> for FailReads<S> {
    type Request = M4aSourceRequest;
    fn pending(decoder: &T) -> EngineResult<Self::Request> {
        S::pending(decoder)
    }
    async fn read(
        &mut self,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
    ) -> EngineResult<SourceRead<impl AsRef<[u8]> + Send + 'static>> {
        if self.fail.load(Ordering::Acquire) {
            return Err(invalid("injected media failure after acceptance"));
        }
        self.source.read(tx, state).await
    }
    async fn resume(
        &mut self,
        request: Self::Request,
        decoder: OffWorker<Option<T>>,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
    ) -> EngineResult<Option<SourceResume<T>>> {
        self.source.resume(request, decoder, tx, state).await
    }
}
impl<T: Decoder + M4aSeekDecoder, S: SeekSource<T>> SeekSource<T> for FailReads<S> {
    async fn resume_before(
        &mut self,
        request: M4aSourceRequest,
        decoder: OffWorker<Option<T>>,
        context: SeekContext<'_, '_>,
    ) -> EngineResult<Option<SourceResume<T>>> {
        self.source.resume_before(request, decoder, context).await
    }
}
#[tokio::test]
async fn errors_after_an_accepted_seek_use_the_active_revision() {
    let data = fixture("1-seek-front.m4a");
    let full = candidate::decode(&data, 16 * 1024 * 1024).unwrap();
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let flag = Arc::new(AtomicBool::new(false));
    let source = FailReads {
        source: file_source(&owner, "1-seek-front.m4a").await,
        fail: flag.clone(),
    };
    let (tx, mut rx) = stream_channel();
    let (commands, controls) = channel();
    let state = StreamingState::new_reusable();
    let _cancel = Cancel(state.clone());
    let task = spawn::<false, _>(
        owner,
        source,
        data.len() as u64,
        full.sample_rate,
        tx,
        state.clone(),
        controls,
    );
    receive(&mut rx, StreamRevision::INITIAL).await;
    let value = prepared(request(&commands, Timestamp::ZERO, deadline()).unwrap()).await;
    flag.store(true, Ordering::Release);
    let revision = acknowledge(value, &state, 0);
    loop {
        let message = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.revision(), revision);
        match message {
            StreamMsg::Error { message, .. } => {
                assert!(message.contains("injected media failure"));
                break;
            }
            StreamMsg::Done { .. } => panic!("failed source published Done"),
            _ => {}
        }
    }
    assert!(task.await.unwrap().is_err());
}
#[tokio::test]
async fn control_requests_are_processed_in_order_and_cannot_skip_an_acceptance() {
    let data = fixture("1-seek-front.m4a");
    let full = candidate::decode(&data, 16 * 1024 * 1024).unwrap();
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let source = file_source(&owner, "1-seek-front.m4a").await;
    let (tx, mut rx) = stream_channel();
    let (commands, controls) = channel();
    let state = StreamingState::new_reusable();
    let _cancel = Cancel(state.clone());
    let task = spawn::<true, _>(
        owner,
        source,
        data.len() as u64,
        full.sample_rate,
        tx,
        state.clone(),
        controls,
    );
    receive(&mut rx, StreamRevision::INITIAL).await;
    let first = request(&commands, Timestamp::new(1000), deadline()).unwrap();
    let mut second = request(&commands, Timestamp::ZERO, deadline()).unwrap();
    let first = prepared(first).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(5), &mut second)
            .await
            .is_err()
    );
    let first_rev = acknowledge(first, &state, 1000);
    let second = prepared(second).await;
    assert_eq!(second.revision, first_rev.checked_next().unwrap());
    let second_rev = acknowledge(second, &state, 0);
    assert_pcm(&receive(&mut rx, second_rev).await, &full.samples);
    stop(task, &state).await;
}
#[tokio::test]
async fn prepared_receipt_checks_player_source_identity_before_clearing_pcm() {
    let data = fixture("1-seek-front.m4a");
    let full = candidate::decode(&data, 16 * 1024 * 1024).unwrap();
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let source = file_source(&owner, "1-seek-front.m4a").await;
    let (tx, mut rx) = stream_channel();
    let (commands, controls) = channel();
    let state = StreamingState::new_reusable();
    let _cancel = Cancel(state.clone());
    let task = spawn::<true, _>(
        owner,
        source,
        data.len() as u64,
        full.sample_rate,
        tx,
        state.clone(),
        controls,
    );
    receive(&mut rx, StreamRevision::INITIAL).await;
    let value = prepared(request(&commands, Timestamp::ZERO, deadline()).unwrap()).await;
    // All other acceptance preconditions must hold. An unloaded player would
    // reject even if source identity validation disappeared.
    let foreign_state = StreamingState::new_reusable();
    if let Some(presentation) = state.presentation() {
        foreign_state.bind_presentation(presentation).unwrap();
    } else if let Some(timeline) = state.timeline() {
        foreign_state.bind_timeline(timeline).unwrap();
    }
    let (foreign_tx, foreign_rx) = stream_channel();
    let mut foreign = InnerAudioPlayer::new(887, full.channels);
    foreign
        .start_streaming(
            "foreign loaded source".into(),
            foreign_rx,
            foreign_state.clone(),
        )
        .unwrap();
    assert!(
        foreign_tx
            .try_send(StreamMsg::Ready {
                revision: StreamRevision::INITIAL,
                sample_rate: full.sample_rate,
                channels: full.channels,
            })
            .is_ok()
    );
    let mut pool = crate::streaming::PcmPool::new();
    let mut pcm = pool.take();
    pcm.buffer_mut().extend_from_slice(&full.samples);
    assert!(
        foreign_tx
            .try_send(StreamMsg::Samples {
                revision: StreamRevision::INITIAL,
                pcm,
            })
            .is_ok()
    );
    assert!(
        foreign_tx
            .try_send(StreamMsg::Done {
                revision: StreamRevision::INITIAL,
            })
            .is_ok()
    );
    foreign.poll_stream();
    assert!(foreign.shared.is_loaded());
    foreign.play();
    assert!(foreign.process(&mut vec![0.0; 5 * full.channels as usize]));
    let before = (
        foreign.shared.position_frames(),
        foreign.physical_pcm_bytes(),
        foreign.shared.duration(),
    );
    assert!(before.0 > 0 && before.1 > 0);
    foreign.drain_events().for_each(drop);
    assert!(value.accept(&mut foreign, 0).is_err());
    assert_eq!(
        (
            foreign.shared.position_frames(),
            foreign.physical_pcm_bytes(),
            foreign.shared.duration()
        ),
        before
    );
    assert!(foreign.shared.is_loaded());
    assert!(!foreign_state.is_cancelled());
    assert_eq!(foreign.drain_events().count(), 0);
    task.await.unwrap().unwrap();
    assert!(state.is_cancelled());
}

#[tokio::test]
async fn rejected_control_during_a_pending_http_body_recovers_the_exact_wait() {
    let data = fixture("1-seek-front.m4a");
    let full = candidate::decode(&data, 16 * 1024 * 1024).unwrap();
    let (server, source) = http_source::<true>(
        data.clone(),
        Config {
            hold: Some(128),
            ..Config::default()
        },
    )
    .await;
    let waiting = Arc::new(Notify::new());
    let source = ObservedRead {
        source,
        reads: 0,
        waiting: waiting.clone(),
    };
    let (tx, mut rx) = stream_channel();
    let (commands, controls) = channel();
    let state = StreamingState::new_reusable();
    let _cancel = Cancel(state.clone());
    let task = spawn::<true, _>(
        migo_io::scheduler::IoScheduler::new(0),
        source,
        data.len() as u64,
        full.sample_rate,
        tx,
        state.clone(),
        controls,
    );
    tokio::time::timeout(Duration::from_secs(3), waiting.notified())
        .await
        .expect("second actual HTTP read never became pending");
    let reply = request(&commands, Timestamp::new(i64::MAX), deadline()).unwrap();
    assert!(matches!(
        reply.await.unwrap(),
        Reply::Rejected(_) | Reply::NotStarted(_)
    ));
    server.release.notify_one();
    assert_pcm(
        &receive(&mut rx, StreamRevision::INITIAL).await,
        &full.samples,
    );
    assert_eq!(server.full_requests.load(Ordering::Acquire), 1);
    stop(task, &state).await;
}
#[tokio::test]
async fn cached_eof_seek_drains_retained_decoder_before_reading_the_source_again() {
    let name = "1-seek-front.m4a";
    let data = fixture(name);
    let full = candidate::decode(&data, 16 * 1024 * 1024).unwrap();
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let path = std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join(name);
    let reader = FileReader::open(std::fs::File::open(path).unwrap(), &owner)
        .await
        .unwrap();
    let reads = reader.io_reads.clone();
    let (tx, mut rx) = stream_channel();
    let (commands, controls) = channel();
    let state = StreamingState::new_reusable();
    let _cancel = Cancel(state.clone());
    let task = spawn::<true, _>(
        owner,
        IndexedFile::from_reader(reader),
        data.len() as u64,
        full.sample_rate,
        tx,
        state.clone(),
        controls,
    );
    receive(&mut rx, StreamRevision::INITIAL).await;
    let before = reads.load(Ordering::Acquire);
    let value = prepared(request(&commands, Timestamp::ZERO, deadline()).unwrap()).await;
    let revision = acknowledge(value, &state, 0);
    assert_pcm(&receive(&mut rx, revision).await, &full.samples);
    assert_eq!(
        reads.load(Ordering::Acquire),
        before,
        "cached seek performed extra source IO"
    );
    stop(task, &state).await;
}
#[tokio::test]
async fn prepared_acceptance_uses_the_original_absolute_deadline() {
    let data = fixture("1-seek-front.m4a");
    let full = candidate::decode(&data, 16 * 1024 * 1024).unwrap();
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let source = file_source(&owner, "1-seek-front.m4a").await;
    let (tx, mut rx) = stream_channel();
    let (commands, controls) = channel();
    let state = StreamingState::new_reusable();
    let _cancel = Cancel(state.clone());
    let task = spawn::<true, _>(
        owner,
        source,
        data.len() as u64,
        full.sample_rate,
        tx,
        state.clone(),
        controls,
    );
    receive(&mut rx, StreamRevision::INITIAL).await;
    let _value = prepared(
        request(
            &commands,
            Timestamp::ZERO,
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap(),
    )
    .await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(2)).await;
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    assert!(
        state.is_cancelled(),
        "acceptance renewed the request deadline"
    );
    task.await.unwrap().unwrap();
}
#[tokio::test]
async fn ordinary_consumers_cannot_accidentally_enable_a_persistent_source() {
    let trace = Arc::new(Trace::default());
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let source = MemorySource {
        trace: trace.clone(),
        sent: false,
    };
    let (tx, mut rx) = stream_channel();
    let (_commands, controls) = channel();
    let state = StreamingState::new();
    let copy = trace.clone();
    let operation = transport::run_source_controlled(
        owner.audio_cpu(),
        source,
        Some(3),
        tx.clone(),
        state.clone(),
        move |_| {
            copy.created.fetch_add(1, Ordering::Relaxed);
            Tiny { trace: copy }
        },
        controls,
    );
    tokio::pin!(operation);
    let result = std::future::poll_fn(|cx| match operation.as_mut().poll(cx) {
        std::task::Poll::Ready(result) => std::task::Poll::Ready(result),
        std::task::Poll::Pending => panic!("ordinary consumer started persistent work"),
    })
    .await;
    assert!(result.is_err());
    assert_eq!(trace.reads.load(Ordering::Acquire), 0);
    assert_eq!(trace.created.load(Ordering::Acquire), 0);
    assert!(matches!(
        rx.try_recv().unwrap(),
        StreamMsg::Error {
            revision: StreamRevision::INITIAL,
            ..
        }
    ));
}

struct HeldRead {
    trace: Arc<Trace>,
    sent: bool,
    waiting: Arc<Notify>,
}
impl Source<Tiny> for HeldRead {
    type Request = M4aSourceRequest;
    fn pending(_decoder: &Tiny) -> EngineResult<Self::Request> {
        Err(invalid("unexpected source request"))
    }
    async fn read(
        &mut self,
        _tx: &mpsc::Sender<StreamMsg>,
        _state: &Arc<StreamingState>,
    ) -> EngineResult<SourceRead<impl AsRef<[u8]> + Send + 'static>> {
        self.trace.reads.fetch_add(1, Ordering::Relaxed);
        if !self.sent {
            self.sent = true;
            return Ok(SourceRead::Data(vec![1, 2, 3]));
        }
        self.waiting.notify_one();
        std::future::pending().await
    }
    async fn resume(
        &mut self,
        _request: Self::Request,
        _decoder: OffWorker<Option<Tiny>>,
        _tx: &mpsc::Sender<StreamMsg>,
        _state: &Arc<StreamingState>,
    ) -> EngineResult<Option<SourceResume<Tiny>>> {
        Err(invalid("unexpected source resume"))
    }
}
struct Refusals {
    requests: mpsc::Receiver<oneshot::Sender<()>>,
}
impl SourceControl<Tiny, HeldRead> for Refusals {
    type Request = oneshot::Sender<()>;
    fn enabled(&self) -> bool {
        true
    }
    fn recover_read(&mut self, _source: &mut HeldRead) -> EngineResult<()> {
        Ok(())
    }
    async fn next(&mut self) -> Option<Self::Request> {
        self.requests.recv().await
    }
    async fn apply<'a>(
        &'a mut self,
        reply: Self::Request,
        _source: &'a mut HeldRead,
        decoder: OffWorker<Option<Tiny>>,
        _pcm: PcmChunk,
        _context: ControlContext<'a>,
    ) -> EngineResult<Option<ControlResult<Tiny>>> {
        let _ = reply.send(());
        Ok(Some(ControlResult::Unchanged(decoder)))
    }
}
#[tokio::test]
async fn rejected_controls_do_not_renew_a_stalled_media_reads_idle_deadline() {
    let trace = Arc::new(Trace::default());
    let waiting = Arc::new(Notify::new());
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let source = HeldRead {
        trace: trace.clone(),
        sent: false,
        waiting: waiting.clone(),
    };
    let (tx, mut rx) = stream_channel();
    let (requests, controls) = mpsc::channel(4);
    let state = StreamingState::new_reusable();
    let _cancel = Cancel(state.clone());
    let task = ActorTask {
        handle: tokio::spawn(transport::run_source_controlled(
            owner.audio_cpu(),
            source,
            None,
            tx,
            state.clone(),
            move |_| Tiny { trace },
            Refusals { requests: controls },
        )),
        _owner: owner,
    };
    tokio::time::timeout(Duration::from_secs(2), waiting.notified())
        .await
        .unwrap();
    while let Ok(message) = rx.try_recv() {
        assert_eq!(message.revision(), StreamRevision::INITIAL);
    }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(15)).await;
    let (reply, result) = oneshot::channel();
    requests.send(reply).await.unwrap();
    result.await.unwrap();
    tokio::time::advance(Duration::from_secs(16)).await;
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    assert!(
        task.is_finished(),
        "refused request renewed stalled body deadline"
    );
    assert_eq!(task.await.unwrap().err().unwrap().code, ErrorCode::Timeout);
    assert!(matches!(
        rx.try_recv().unwrap(),
        StreamMsg::Error {
            revision: StreamRevision::INITIAL,
            ..
        }
    ));
}
#[tokio::test]
async fn the_actual_player_accepts_the_receipt_then_waits_for_fresh_pcm() {
    let data = fixture("2-front.m4a");
    let full = candidate::decode(&data, 16 * 1024 * 1024).unwrap();
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let source = file_source(&owner, "2-front.m4a").await;
    let (tx, rx) = stream_channel();
    let (commands, controls) = channel();
    let state = StreamingState::new_reusable();
    let _cancel = Cancel(state.clone());
    let mut player = InnerAudioPlayer::new(888, 2);
    let load = player.begin_load_for(1).unwrap();
    player
        .complete_stream_load(load, "producer".into(), rx, state.clone())
        .unwrap();
    let task = spawn::<true, _>(
        owner,
        source,
        data.len() as u64,
        full.sample_rate,
        tx,
        state.clone(),
        controls,
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        while !player.shared.is_loaded() {
            player.poll_stream();
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    player.drain_events().for_each(drop);
    let value = prepared(request(&commands, Timestamp::new(1000), deadline()).unwrap()).await;
    value.accept(&mut player, 1000).unwrap();
    assert_eq!(player.shared.position_frames(), 1000);
    assert!(
        player
            .drain_events()
            .all(|event| event.event_type
                != shared::protocol::audio_cmd::InnerAudioEventType::Seeked)
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            player.poll_stream();
            if player.drain_events().any(|event| {
                event.event_type == shared::protocol::audio_cmd::InnerAudioEventType::Seeked
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    stop(task, &state).await;
}

#[test]
fn an_expired_receipt_rejects_before_mutating_pcm_even_when_the_waiter_has_not_polled() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    let state = StreamingState::new_reusable();
    let (tx, rx) = stream_channel();
    let mut player = InnerAudioPlayer::new(777, 1);
    player
        .start_streaming("unpolled receipt".into(), rx, state.clone())
        .unwrap();
    assert!(
        tx.try_send(StreamMsg::Ready {
            revision: StreamRevision::INITIAL,
            sample_rate: 8000,
            channels: 1,
        })
        .is_ok()
    );
    let mut pool = crate::streaming::PcmPool::new();
    let mut pcm = pool.take();
    pcm.buffer_mut().extend_from_slice(&[0.25, 0.5]);
    assert!(
        tx.try_send(StreamMsg::Samples {
            revision: StreamRevision::INITIAL,
            pcm,
        })
        .is_ok()
    );
    assert!(
        tx.try_send(StreamMsg::Done {
            revision: StreamRevision::INITIAL,
        })
        .is_ok()
    );
    player.poll_stream();
    assert!(player.shared.is_loaded());
    let before = (
        player.shared.position_frames(),
        player.shared.duration_frames(),
        player.physical_pcm_bytes(),
    );
    player.drain_events().for_each(drop);
    let (accept, mut waiter) = oneshot::channel();
    let value = Prepared {
        traversal: None,
        revision: StreamRevision::INITIAL.checked_next().unwrap(),
        target: 0,
        source: state.clone(),
        deadline: Instant::now() - Duration::from_secs(1),
        accept,
    };
    assert!(
        !value.accept.is_closed(),
        "waiter must still be open and unpolled"
    );
    assert_eq!(
        value.accept(&mut player, 1000).unwrap_err().code,
        ErrorCode::Timeout
    );
    assert_eq!(
        (
            player.shared.position_frames(),
            player.shared.duration_frames(),
            player.physical_pcm_bytes()
        ),
        before
    );
    assert_eq!(player.drain_events().count(), 0);
    assert!(state.is_cancelled());
    assert!(matches!(
        waiter.try_recv(),
        Err(oneshot::error::TryRecvError::Closed)
    ));
}
