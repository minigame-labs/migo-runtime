//! Actual codecs share one local source, AudioCPU, PCM pool and player.
use crate::streaming_player_contracts as playback;
use crate::{
    local_source::FileReader,
    push_decoder::{self, incremental::Packetizer},
    resampled_streaming::{Batched, core},
    streaming::{StreamMsg, StreamingState, stream_channel},
    streaming_transport::{self as transport, Decoder, Source, SourceRead, Step},
};
use migo_io::scheduler::IoScheduler;
use rubato::{Resampler, audioadapter::Adapter, audioadapter_buffers::direct::InterleavedSlice};
use shared::{
    error::EngineResult,
    vfs::{DirSource, MountTable, PackSource, PackageWriter, VirtualFS},
};
use std::{
    fs::File,
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::mpsc;
#[derive(Clone, Copy, Debug)]
enum Backend {
    File,
    Code,
    Directory,
    Package,
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum Format {
    Mp3,
    Wave,
    Adts,
    Vorbis,
}
pub(crate) fn assets() -> PathBuf {
    PathBuf::from(std::env::var_os("W09_LOCAL_SOURCE_ASSETS").unwrap())
}
fn package() -> Arc<MountTable> {
    static VALUE: OnceLock<Arc<MountTable>> = OnceLock::new();
    VALUE
        .get_or_init(|| {
            let path = assets().parent().unwrap().join("assets.mpkg");
            let mut writer =
                PackageWriter::new(std::io::BufWriter::new(File::create(&path).unwrap())).unwrap();
            for entry in std::fs::read_dir(assets()).unwrap() {
                let entry = entry.unwrap();
                writer
                    .add_entry(
                        entry.file_name().to_str().unwrap(),
                        &std::fs::read(entry.path()).unwrap(),
                    )
                    .unwrap();
            }
            writer.finish("game", "1").unwrap();
            let mt = Arc::new(MountTable::new(assets()));
            mt.swap_base(Arc::new(PackSource::open(&path, "game", "1").unwrap()));
            mt
        })
        .clone()
}
async fn open(name: &str, backend: Backend, owner: &IoScheduler, fragment: usize) -> FileReader {
    let mut reader = match backend {
        Backend::File => FileReader::open(File::open(assets().join(name)).unwrap(), owner).await,
        Backend::Code => {
            let cache = assets().parent().unwrap().join("cache");
            FileReader::open_code(
                Arc::new(VirtualFS::new(
                    assets(),
                    cache.clone(),
                    cache.clone(),
                    cache,
                )),
                format!("/code/{name}"),
                owner,
            )
            .await
        }
        Backend::Directory => {
            let mt = MountTable::new(assets());
            assert!(mt.mount_overlay(
                "local".into(),
                "sub".into(),
                Arc::new(DirSource::try_new(assets()).unwrap())
            ));
            FileReader::open_directory(
                mt.pin_directory_read(&format!("sub/{name}"), mt.generation())
                    .unwrap(),
                owner,
            )
            .await
        }
        Backend::Package => {
            let mt = package();
            FileReader::open_package(mt.pin_pack_read(name, mt.generation()).unwrap(), owner).await
        }
    }
    .unwrap();
    reader.chunk_bytes = fragment;
    reader
}
struct Observed<P: Packetizer> {
    decoder: Batched<16, P>,
    io_thread: std::thread::ThreadId,
    steps: Arc<AtomicUsize>,
}
impl<P: Packetizer> Decoder for Observed<P> {
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
            "codec/DSP ran on IO thread"
        );
        self.steps.fetch_add(1, Ordering::AcqRel);
        self.decoder.step(input, eof, pcm, cancel)
    }
}
async fn produce<P: Packetizer>(
    owner: IoScheduler,
    reader: FileReader,
    target: u32,
    tx: mpsc::Sender<StreamMsg>,
    state: Arc<StreamingState>,
    steps: Arc<AtomicUsize>,
) {
    let io_thread = std::thread::current().id();
    let length = reader.len();
    let result = transport::run_source(
        owner.audio_cpu(),
        reader,
        Some(length),
        &tx,
        &state,
        move |_| Observed::<P> {
            decoder: Batched::new(target),
            io_thread,
            steps,
        },
    )
    .await;
    transport::publish_result(&tx, &state, result).await;
}
pub(crate) fn spawn(
    format: Format,
    owner: IoScheduler,
    reader: FileReader,
    target: u32,
    tx: mpsc::Sender<StreamMsg>,
    state: Arc<StreamingState>,
    steps: Arc<AtomicUsize>,
) -> tokio::task::JoinHandle<()> {
    match format {
        Format::Mp3 => tokio::spawn(produce::<push_decoder::Mp3>(
            owner, reader, target, tx, state, steps,
        )),
        Format::Wave => tokio::spawn(produce::<push_decoder::Wave>(
            owner, reader, target, tx, state, steps,
        )),
        Format::Adts => tokio::spawn(produce::<push_decoder::Adts>(
            owner, reader, target, tx, state, steps,
        )),
        Format::Vorbis => tokio::spawn(produce::<push_decoder::Vorbis>(
            owner, reader, target, tx, state, steps,
        )),
    }
}
pub(crate) fn reference(data: &[u8], target: u32) -> (Vec<f32>, u32) {
    let decoded = crate::candidate::decode(data, 16_777_216).unwrap();
    let channels = decoded.channels;
    if decoded.sample_rate == target {
        return (decoded.samples, channels);
    }
    let frames = decoded.samples.len() / channels as usize;
    let input = InterleavedSlice::new(&decoded.samples, channels as usize, frames).unwrap();
    let result = core(decoded.sample_rate, target, channels)
        .unwrap()
        .process_all(&input, frames, None)
        .unwrap();
    let count = (frames as u64 * target as u64).div_ceil(decoded.sample_rate as u64) as usize;
    let mut out = Vec::with_capacity(count * channels as usize);
    for frame in 0..count {
        for ch in 0..channels as usize {
            out.push(result.read_sample(ch, frame).unwrap());
        }
    }
    (out, channels)
}
pub(crate) struct Received {
    pub pcm: Vec<f32>,
    pub ready: usize,
    pub done: usize,
    pub errors: Vec<String>,
}
pub(crate) async fn receive(
    format: Format,
    owner: IoScheduler,
    reader: FileReader,
    target: u32,
) -> Received {
    let state = StreamingState::new();
    let (tx, mut rx) = stream_channel();
    let task = spawn(
        format,
        owner,
        reader,
        target,
        tx,
        state.clone(),
        Arc::new(AtomicUsize::new(0)),
    );
    let mut got = Received {
        pcm: vec![],
        ready: 0,
        done: 0,
        errors: vec![],
    };
    while let Some(msg) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
    {
        match msg {
            StreamMsg::Ready { sample_rate, .. } => {
                assert_eq!(sample_rate, target);
                got.ready += 1;
            }
            StreamMsg::Samples(pcm) => {
                assert_eq!((got.ready, got.done, got.errors.len()), (1, 0, 0));
                got.pcm.extend_from_slice(&pcm);
            }
            StreamMsg::Done => got.done += 1,
            StreamMsg::Error(e) => got.errors.push(e),
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
async fn every_format_backend_partition_and_rate_matches_whole_pcm() {
    let mut count = 0;
    for (format, ext) in [
        (Format::Mp3, "mp3"),
        (Format::Wave, "wav"),
        (Format::Adts, "aac"),
        (Format::Vorbis, "ogg"),
    ] {
        for i in 0..4 {
            let name = format!("{i}.{ext}");
            let data = std::fs::read(assets().join(&name)).unwrap();
            let rate = crate::candidate::decode(&data, 16_777_216)
                .unwrap()
                .sample_rate;
            for target in [rate, if rate == 48000 { 44100 } else { 48000 }] {
                let (expected, _) = reference(&data, target);
                for backend in [
                    Backend::File,
                    Backend::Code,
                    Backend::Directory,
                    Backend::Package,
                ] {
                    for fragment in [137, 65536] {
                        let owner = IoScheduler::new(0);
                        let reader = open(&name, backend, &owner, fragment).await;
                        let got = receive(format, owner, reader, target).await;
                        assert_eq!(
                            (got.ready, got.done, got.errors.len()),
                            (1, 1, 0),
                            "{name}/{backend:?}/{target}/{fragment}: {:?}",
                            got.errors
                        );
                        assert_eq!(got.pcm, expected, "{name}/{backend:?}/{target}/{fragment}");
                        count += 1;
                    }
                }
            }
        }
    }
    assert_eq!(count, 256);
    println!("W09_LOCAL_FORMAT_PCM {count} exact comparisons");
}
#[tokio::test]
async fn every_format_player_preserves_pause_pcm_and_exact_ended() {
    let mut count = 0;
    for (format, name) in [
        (Format::Mp3, "3.mp3"),
        (Format::Wave, "3.wav"),
        (Format::Adts, "3.aac"),
        (Format::Vorbis, "2.ogg"),
    ] {
        for backend in [Backend::Code, Backend::Package] {
            for target in [44100, 48000] {
                let (expected, channels) =
                    reference(&std::fs::read(assets().join(name)).unwrap(), target);
                let frames = expected.len() / channels as usize;
                let owner = IoScheduler::new(0);
                let reader = open(name, backend, &owner, 65536).await;
                let state = StreamingState::new();
                let (tx, rx) = stream_channel();
                let steps = Arc::new(AtomicUsize::new(0));
                let task = spawn(
                    format,
                    owner,
                    reader,
                    target,
                    tx,
                    state.clone(),
                    steps.clone(),
                );
                playback::consume_player::<false>(
                    crate::streaming_player_contracts::PlaybackCase {
                        name,
                        source_name: format!("{backend:?}://{name}"),
                        target,
                        pause: true,
                        long: false,
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
                count += 1;
            }
        }
    }
    assert_eq!(count, 16);
    println!("W09_LOCAL_FORMAT_PLAYER {count} cases");
}
#[tokio::test]
async fn first_pcm_and_four_slot_backpressure_do_not_collect_a_whole_file() {
    for backend in [Backend::Code, Backend::Package] {
        let owner = IoScheduler::new(0);
        let reader = open("3.wav", backend, &owner, 65536).await;
        let whole_reads = reader.len().div_ceil(65536);
        assert!(whole_reads > 16);
        let reads = reader.io_reads.clone();
        let state = StreamingState::new();
        let (tx, mut rx) = stream_channel();
        let steps = Arc::new(AtomicUsize::new(0));
        let task = spawn(
            Format::Wave,
            owner,
            reader,
            48000,
            tx,
            state.clone(),
            steps.clone(),
        );
        let mut samples = false;
        while !samples {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap()
            {
                StreamMsg::Samples(_) => samples = true,
                StreamMsg::Ready { .. } => {}
                _ => panic!("no first PCM"),
            }
        }
        assert!(
            (reads.load(Ordering::Acquire) as u64) < whole_reads,
            "source collected whole file before first PCM"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        let before = (steps.load(Ordering::Acquire), reads.load(Ordering::Acquire));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            (steps.load(Ordering::Acquire), reads.load(Ordering::Acquire)),
            before,
            "full PCM channel drained source"
        );
        state.cancel();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert!(!state.download_complete.load(Ordering::Acquire));
    }
}
#[tokio::test]
async fn sequential_chunks_retain_the_existing_two_slot_pool_owner() {
    let owner = IoScheduler::new(0);
    let mut reader = open("3.wav", Backend::File, &owner, 65536).await;
    let (tx, _rx) = stream_channel();
    let state = StreamingState::new();
    let a = <FileReader as Source<Batched<16>>>::read(&mut reader, &tx, &state)
        .await
        .unwrap();
    let b = <FileReader as Source<Batched<16>>>::read(&mut reader, &tx, &state)
        .await
        .unwrap();
    assert!(matches!(a, SourceRead::Data(_)) && matches!(b, SourceRead::Data(_)));
    assert!(
        <FileReader as Source<Batched<16>>>::read(&mut reader, &tx, &state)
            .await
            .is_err(),
        "source copied and released pooled chunks"
    );
    drop((a, b));
    assert!(reader.chunk().await.is_err(), "failed reader reused");
}
#[tokio::test]
async fn every_format_truncated_or_failed_io_publishes_error_without_done() {
    for (format, name) in [
        (Format::Mp3, "bad.mp3"),
        (Format::Wave, "bad.wav"),
        (Format::Adts, "bad.aac"),
        (Format::Vorbis, "bad.ogg"),
    ] {
        for backend in [Backend::File, Backend::Directory, Backend::Package] {
            let owner = IoScheduler::new(0);
            let reader = open(name, backend, &owner, 137).await;
            let got = receive(format, owner, reader, 48000).await;
            assert_eq!((got.done, got.errors.len()), (0, 1), "{name}/{backend:?}");
        }
    }
    for backend in [Backend::File, Backend::Package] {
        let owner = IoScheduler::new(0);
        let mut reader = open("3.wav", backend, &owner, 65536).await;
        reader.delay = Duration::from_millis(200);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), reader.chunk())
                .await
                .is_err()
        );
        let got = receive(Format::Wave, owner, reader, 48000).await;
        assert_eq!(
            (got.ready, got.done, got.errors.len()),
            (0, 0, 1),
            "failed IO became EOF"
        );
    }
}
