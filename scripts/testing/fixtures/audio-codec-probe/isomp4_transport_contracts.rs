//! Real M4A -> common CPU/DSP/PCM pool -> production player, without a device.
use super::*;
use crate::{
    isomp4_transport::IndexedHttp,
    push_decoder::{M4a, M4aSourceDecoder},
    resampled_streaming::{Batched, core},
    streaming_transport::{self as transport, Decoder, Progress as DspProgress, Step},
};
use rubato::{Resampler, audioadapter::Adapter, audioadapter_buffers::direct::InterleavedSlice};
use shared::error::EngineResult;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::streaming_player_contracts::consume_player;

struct Observed<const B: bool> {
    decoder: Batched<16, M4a<B>>,
    steps: Arc<AtomicUsize>,
    io_thread: std::thread::ThreadId,
}
impl<const B: bool> Decoder for Observed<B> {
    fn step(
        &mut self,
        input: &[u8],
        eof: bool,
        pcm: &mut Vec<f32>,
        cancel: &AtomicBool,
    ) -> EngineResult<Step> {
        assert_ne!(
            std::thread::current().id(),
            self.io_thread,
            "DSP ran on IO thread"
        );
        self.steps.fetch_add(1, Ordering::AcqRel);
        self.decoder.step(input, eof, pcm, cancel)
    }
}
impl<const B: bool> M4aSourceDecoder for Observed<B> {
    fn source_request(&self) -> Option<M4aSourceRequest> {
        self.decoder.source_request()
    }
    fn install_index_with_length(
        &mut self,
        index: AudioIndex,
        length: Option<u64>,
    ) -> Result<(), String> {
        self.decoder.install_index_with_length(index, length)
    }
    fn resume_at(&mut self, media: AtomSpan, offset: u64) -> Result<(), String> {
        self.decoder.resume_at(media, offset)
    }
}
async fn produce<const B: bool>(
    url: String,
    target: u32,
    tx: mpsc::Sender<StreamMsg>,
    state: Arc<StreamingState>,
    steps: Arc<AtomicUsize>,
) {
    produce_from::<B, IndexedHttp>(url, target, tx, state, steps).await;
}
async fn produce_from<const B: bool, S: transport::HttpSource<Observed<B>> + 'static>(
    url: String,
    target: u32,
    tx: mpsc::Sender<StreamMsg>,
    state: Arc<StreamingState>,
    steps: Arc<AtomicUsize>,
) {
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let io_thread = std::thread::current().id();
    transport::run_with_source::<Observed<B>, S>(
        owner.audio_cpu(),
        client(),
        url,
        tx,
        state,
        move |length| Observed {
            decoder: Batched::with_object_len(target, length),
            steps,
            io_thread,
        },
    )
    .await;
}
fn reference(data: &[u8], target: u32) -> (Vec<f32>, u32) {
    let audio = candidate::decode(data, 16_777_216).unwrap();
    if audio.sample_rate == target {
        return (audio.samples, audio.channels);
    }
    let channels = audio.channels as usize;
    let frames = audio.samples.len() / channels;
    let source = InterleavedSlice::new(&audio.samples, channels, frames).unwrap();
    let output = core(audio.sample_rate, target, audio.channels)
        .unwrap()
        .process_all(&source, frames, None)
        .unwrap();
    let frames = (frames as u64 * target as u64).div_ceil(audio.sample_rate as u64) as usize;
    let mut pcm = Vec::with_capacity(frames * channels);
    for frame in 0..frames {
        for ch in 0..channels {
            pcm.push(output.read_sample(ch, frame).unwrap());
        }
    }
    (pcm, audio.channels)
}
struct Received {
    pcm: Vec<f32>,
    ready: usize,
    done: usize,
    errors: Vec<String>,
}
async fn receive<const B: bool>(server: &Server, target: u32) -> Received {
    receive_from::<B, IndexedHttp>(server, target).await
}
async fn receive_from<const B: bool, S: transport::HttpSource<Observed<B>> + 'static>(
    server: &Server,
    target: u32,
) -> Received {
    let state = StreamingState::new();
    let (tx, mut rx) = stream_channel();
    let task = tokio::spawn(produce_from::<B, S>(
        server.url.as_str().to_owned(),
        target,
        tx,
        state.clone(),
        Arc::new(AtomicUsize::new(0)),
    ));
    let mut got = Received {
        pcm: Vec::new(),
        ready: 0,
        done: 0,
        errors: Vec::new(),
    };
    while let Some(message) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("M4A publication stalled")
    {
        match message {
            StreamMsg::Ready {
                sample_rate,
                channels,
            } => {
                assert_eq!(sample_rate, target);
                assert!(channels == 1 || channels == 2);
                got.ready += 1;
            }
            StreamMsg::Samples(pcm) => {
                assert_eq!((got.ready, got.done, got.errors.len()), (1, 0, 0));
                assert!(pcm.len() <= crate::bandlimited::MAX_SCRATCH_SAMPLES);
                got.pcm.extend_from_slice(&pcm);
            }
            StreamMsg::Done => got.done += 1,
            StreamMsg::Error(error) => got.errors.push(error),
        }
    }
    task.await.unwrap();
    assert_eq!(
        state.download_complete.load(Ordering::Acquire),
        got.done == 1
    );
    got
}
#[tokio::test]
async fn http_index_rewinds_and_buffered_sources_keep_exact_dsp_pcm() {
    let mut comparisons = 0;
    for name in [
        "0-front.m4a",
        "1-tail.m4a",
        "2-reorder.m4a",
        "2-tail-reverse.m4a",
        "2-tail-multi.m4a",
        "1-metadata.m4a",
        "2-buffered-miss.m4a",
    ] {
        let data = fixture(name);
        let source = candidate::decode(&data, 16_777_216).unwrap();
        for target in [
            source.sample_rate,
            if source.sample_rate == 48000 {
                44100
            } else {
                48000
            },
        ] {
            let (expected, _) = reference(&data, target);
            for chunked in [false, true] {
                for mode in [false, true] {
                    let server = start_server(
                        data.clone(),
                        Config {
                            chunked,
                            ..Config::default()
                        },
                    )
                    .await;
                    let got = if mode {
                        receive::<true>(&server, target).await
                    } else {
                        receive::<false>(&server, target).await
                    };
                    assert_eq!(
                        (got.ready, got.done, got.errors.len()),
                        (1, 1, 0),
                        "{name}/{target}/{chunked}/{mode}: {:?}",
                        got.errors
                    );
                    assert_eq!(got.pcm, expected, "{name}/{target}/{chunked}/{mode}");
                    comparisons += 1;
                }
            }
        }
    }
    assert_eq!(comparisons, 56);
    println!("W09_M4A_DSP_HTTP {comparisons} exact PCM comparisons");
}
#[tokio::test]
async fn cached_dsp_needs_no_validator_or_range_and_true_misses_fail_closed() {
    for name in ["1-tail.m4a", "2-tail-multi.m4a"] {
        let data = fixture(name);
        let (expected, _) = reference(&data, 44100);
        let server = start_server(
            data,
            Config {
                no_tag: true,
                fault: Fault::IgnoredRange,
                ..Config::default()
            },
        )
        .await;
        let got = receive::<true>(&server, 44100).await;
        assert_eq!((got.ready, got.done, got.errors.len()), (1, 1, 0));
        assert_eq!(got.pcm, expected);
        assert!(server.ranges.lock().unwrap().is_empty());
    }
    for config in [
        Config {
            no_tag: true,
            ..Config::default()
        },
        Config {
            fault: Fault::ChangedTag,
            ..Config::default()
        },
        Config {
            fault: Fault::IgnoredRange,
            ..Config::default()
        },
        Config {
            cut: Some(64),
            ..Config::default()
        },
    ] {
        let server = start_server(fixture("2-buffered-miss.m4a"), config).await;
        let got = receive::<true>(&server, 44100).await;
        assert_eq!((got.done, got.errors.len()), (0, 1), "{:?}", got.errors);
    }
}
#[tokio::test]
async fn pending_index_wait_cancels_and_receiver_exit_stops_publication() {
    for close in [false, true] {
        let server = start_server(
            fixture("1-tail.m4a"),
            Config {
                fault: Fault::HangHeaders,
                ..Config::default()
            },
        )
        .await;
        let state = StreamingState::new();
        let (tx, rx) = stream_channel();
        let task = tokio::spawn(produce::<false>(
            server.url.as_str().to_owned(),
            44100,
            tx,
            state.clone(),
            Arc::new(AtomicUsize::new(0)),
        ));
        tokio::time::timeout(Duration::from_secs(2), server.index_arrived.notified())
            .await
            .unwrap();
        if close {
            drop(rx);
        } else {
            state.cancel();
        }
        tokio::time::timeout(Duration::from_millis(500), task)
            .await
            .expect("source wait retained CPU/transport")
            .unwrap();
        assert!(state.is_cancelled());
        assert!(!state.download_complete.load(Ordering::Acquire));
    }
}
#[tokio::test]
async fn first_dsp_pcm_arrives_before_initial_body_is_released() {
    let data = fixture("2-front.m4a");
    let media = atom(&data, AtomKind::MediaData);
    let server = start_server(
        data,
        Config {
            hold: Some(media.body_offset as usize + 4096),
            ..Config::default()
        },
    )
    .await;
    let state = StreamingState::new();
    let (tx, mut rx) = stream_channel();
    let task = tokio::spawn(produce::<true>(
        server.url.as_str().to_owned(),
        44100,
        tx,
        state.clone(),
        Arc::new(AtomicUsize::new(0)),
    ));
    let mut ready = false;
    loop {
        match tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("DSP waited for body tail")
            .unwrap()
        {
            StreamMsg::Ready { sample_rate, .. } => {
                assert_eq!(sample_rate, 44100);
                ready = true;
            }
            StreamMsg::Samples(pcm) => {
                assert!(ready && !pcm.is_empty());
                break;
            }
            _ => panic!("held body ended without PCM"),
        }
    }
    assert!(!task.is_finished());
    state.cancel();
    server.release.notify_waiters();
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
}
#[test]
fn bounded_m4a_resampling_steps_have_no_steady_rust_allocation() {
    let data = fixture("long-front.m4a");
    let mut decoder = Batched::<16, M4a<true>>::with_object_len(16000, Some(data.len() as u64));
    let mut out = Vec::with_capacity(crate::bandlimited::MAX_SCRATCH_SAMPLES);
    let cancel = AtomicBool::new(false);
    let mut offset = 0;
    let mut samples = 0;
    migo_alloc_probe::assert_no_steady_state_allocation(
        migo_alloc_probe::Burst {
            path: "M4A shared DSP",
            warmup: 128,
            measured: 128,
        },
        |_| {
            out.clear();
            let step = decoder
                .step(&data[offset..], false, &mut out, &cancel)
                .unwrap();
            offset += step.consumed;
            assert!(!matches!(
                step.progress,
                DspProgress::Finished | DspProgress::NeedSource
            ));
            samples += out.len();
        },
    );
    assert!(samples > 0);
}
#[tokio::test]
async fn four_slot_backpressure_stops_m4a_cpu_and_network_until_cancelled() {
    let server = start_server(fixture("long-front.m4a"), Config::default()).await;
    let state = StreamingState::new();
    let (tx, rx) = stream_channel();
    let steps = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn(produce::<true>(
        server.url.as_str().to_owned(),
        16000,
        tx,
        state.clone(),
        steps.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        while rx.len() != 4 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let before = (
        steps.load(Ordering::Acquire),
        state.bytes_downloaded.load(Ordering::Relaxed),
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        (
            steps.load(Ordering::Acquire),
            state.bytes_downloaded.load(Ordering::Relaxed)
        ),
        before,
        "four-slot backpressure was bypassed"
    );
    assert!(!task.is_finished());
    state.cancel();
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
    assert!(!state.download_complete.load(Ordering::Acquire));
}

async fn play<const B: bool>(name: &str, target: u32, pause: bool, long: bool) {
    let data = fixture(name);
    let (expected, channels) = if long {
        (Vec::new(), 1)
    } else {
        reference(&data, target)
    };
    let frames = if long {
        17_612_800usize * target as usize / 8000
    } else {
        expected.len() / channels as usize
    };
    let server = start_server(data, Config::default()).await;
    let state = StreamingState::new();
    let (tx, rx) = stream_channel();
    let steps = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn(produce::<B>(
        server.url.as_str().to_owned(),
        target,
        tx,
        state.clone(),
        steps.clone(),
    ));
    consume_player::<B>(
        crate::streaming_player_contracts::PlaybackCase {
            name,
            source_name: server.url.as_str().to_owned(),
            target,
            pause,
            long,
            expected,
            channels,
            frames,
        },
        crate::streaming_player_contracts::StreamInput {
            state,
            steps,
            task,
            rx,
        },
    )
    .await;
}

#[tokio::test]
async fn real_player_preserves_pcm_pause_and_exact_ended_across_sources() {
    for (name, mode) in [
        ("2-reorder.m4a", false),
        ("2-tail-multi.m4a", false),
        ("2-tail-multi.m4a", true),
    ] {
        for rate in [48000, 44100] {
            if mode {
                play::<true>(name, rate, true, false).await;
            } else {
                play::<false>(name, rate, true, false).await;
            }
        }
    }
}
#[tokio::test]
async fn long_m4a_player_exceeds_whole_cache_quota_with_bounded_pcm() {
    play::<true>("long-front.m4a", 16000, true, true).await;
}

#[tokio::test]
async fn first_dsp_pcm_from_read_at_does_not_wait_for_media_eof() {
    let data = fixture("2-reverse.m4a");
    let (expected, _) = reference(&data, 44100);
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let (_, packet) = pending(data.clone(), &owner).await;
    let server = start_server(
        data,
        Config {
            hold_media_after: Some(packet.size as usize),
            ..Config::default()
        },
    )
    .await;
    let state = StreamingState::new();
    let (tx, mut rx) = stream_channel();
    let task = tokio::spawn(produce::<false>(
        server.url.as_str().to_owned(),
        44100,
        tx,
        state.clone(),
        Arc::new(AtomicUsize::new(0)),
    ));
    tokio::time::timeout(Duration::from_secs(2), server.media_held.notified())
        .await
        .expect("server did not enter actual media hold");
    let mut actual = Vec::new();
    let (mut ready, mut done) = (0, 0);
    loop {
        match tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("DSP waited for media EOF")
            .unwrap()
        {
            StreamMsg::Ready { sample_rate, .. } => {
                assert_eq!(sample_rate, 44100);
                ready += 1;
            }
            StreamMsg::Samples(pcm) => {
                assert_eq!(ready, 1);
                actual.extend_from_slice(&pcm);
                break;
            }
            _ => panic!("held media ended without PCM"),
        }
    }
    assert!(!task.is_finished());
    server.release.notify_one();
    while let Some(message) = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
    {
        match message {
            StreamMsg::Samples(pcm) => actual.extend_from_slice(&pcm),
            StreamMsg::Done => done += 1,
            _ => panic!("invalid post-resume publication"),
        }
    }
    task.await.unwrap();
    assert_eq!((ready, done), (1, 1));
    assert_eq!(actual, expected);
}
#[test]
fn invalid_source_resume_and_decode_failures_are_sticky() {
    let data = fixture("1-tail.m4a");
    let media = atom(&data, AtomKind::MediaData);
    let mut decoder = Batched::<16, M4a<true>>::with_object_len(44100, Some(data.len() as u64));
    assert!(decoder.resume_at(media, media.body_offset).is_err());
    let mut out = Vec::with_capacity(crate::bandlimited::MAX_SCRATCH_SAMPLES);
    assert!(
        decoder
            .step(&data, false, &mut out, &AtomicBool::new(false))
            .is_err()
    );
    assert!(decoder.source_request().is_none() && out.is_empty());
    let clipped = &data[..data.len() - 3];
    let mut offset = 0;
    let mut failed = false;
    let mut decoder = Batched::<16, M4a<true>>::with_object_len(44100, Some(data.len() as u64));
    for _ in 0..1024 {
        out.clear();
        match decoder.step(
            &clipped[offset..],
            offset == clipped.len(),
            &mut out,
            &AtomicBool::new(false),
        ) {
            Ok(step) => {
                offset += step.consumed;
                assert!(!matches!(
                    step.progress,
                    DspProgress::Finished | DspProgress::NeedSource
                ));
            }
            Err(_) => {
                failed = true;
                break;
            }
        }
    }
    assert!(failed && out.is_empty());
    assert!(
        decoder
            .step(&[], true, &mut out, &AtomicBool::new(false))
            .is_err()
    );
    assert!(decoder.source_request().is_none());
}

// Test-only delivery geometry: coalesce this finite encoded fixture into one
// chunk to force partial acceptance. Product sources never collect this way.
struct CoalescedFixture {
    inner: IndexedHttp,
    first: bool,
}
impl<D: Decoder + M4aSourceDecoder> transport::Source<D> for CoalescedFixture {
    type Request = M4aSourceRequest;
    fn pending(decoder: &D) -> EngineResult<Self::Request> {
        <IndexedHttp as transport::Source<D>>::pending(decoder)
    }
    async fn read(
        &mut self,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
    ) -> EngineResult<transport::SourceRead<impl AsRef<[u8]> + Send + 'static>> {
        use transport::SourceRead;
        if !self.first {
            return Ok(
                match <IndexedHttp as transport::Source<D>>::read(&mut self.inner, tx, state)
                    .await?
                {
                    SourceRead::Data(chunk) => SourceRead::Data(chunk.as_ref().to_vec()),
                    SourceRead::End => SourceRead::End,
                    SourceRead::Cancelled => SourceRead::Cancelled,
                },
            );
        }
        self.first = false;
        let mut data = Vec::new();
        loop {
            match <IndexedHttp as transport::Source<D>>::read(&mut self.inner, tx, state).await? {
                SourceRead::Data(chunk) => {
                    data.extend_from_slice(chunk.as_ref());
                    assert!(data.len() < 2 * 1024 * 1024);
                }
                SourceRead::End => break,
                SourceRead::Cancelled => return Ok(SourceRead::Cancelled),
            }
        }
        assert!(
            data.len() > 65536,
            "fixture must force an unaccepted suffix"
        );
        Ok(SourceRead::Data(data))
    }
    async fn resume(
        &mut self,
        request: Self::Request,
        decoder: OffWorker<Option<D>>,
        tx: &mpsc::Sender<StreamMsg>,
        state: &Arc<StreamingState>,
    ) -> EngineResult<Option<transport::SourceResume<D>>> {
        <IndexedHttp as transport::Source<D>>::resume(&mut self.inner, request, decoder, tx, state)
            .await
    }
}
#[tokio::test]
async fn coalesced_input_preserves_index_suffix_and_replaces_repositioned_suffix() {
    for name in ["1-metadata.m4a", "2-coalesced-rewind.m4a"] {
        let data = fixture(name);
        let (expected, _) = reference(&data, 44100);
        let server = start_server(data, Config::default()).await;
        let got = receive_from::<false, CoalescedFixture>(&server, 44100).await;
        assert_eq!(
            (got.ready, got.done, got.errors.len()),
            (1, 1, 0),
            "{name}: {:?}",
            got.errors
        );
        assert_eq!(got.pcm, expected, "{name}");
        assert!(
            server.ranges.lock().unwrap().len() >= 2,
            "external source path not exercised"
        );
    }
}

impl<D: Decoder + M4aSourceDecoder> transport::HttpSource<D> for CoalescedFixture {
    fn request(
        request: crate::network::http::RequestBuilder,
    ) -> crate::network::http::RequestBuilder {
        <IndexedHttp as transport::HttpSource<D>>::request(request)
    }
    fn new(
        client: crate::network::http::HttpClient,
        response: crate::network::http::Response,
    ) -> EngineResult<Self> {
        Ok(Self {
            inner: <IndexedHttp as transport::HttpSource<D>>::new(client, response)?,
            first: true,
        })
    }
}

#[cfg(feature = "m4a-file-probe")]
#[path = "isomp4_file_contracts.rs"]
pub(crate) mod local;
