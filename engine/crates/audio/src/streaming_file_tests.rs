use crate::inner_audio::{InnerAudioPlayer, PlaybackState};
use crate::streaming::{self, StreamMsg, StreamingState};
use shared::audio_file::AudioFile;
use shared::error::{EngineError, ErrorCode};
use shared::protocol::audio_cmd::{AudioCmd, InnerAudioEventType};
use std::sync::{Arc, atomic::AtomicBool};
use std::{
    fs,
    io::Write,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};
const WAIT: Duration = Duration::from_secs(30);
pub(super) struct Fixture {
    _scope: tokio::sync::OwnedSemaphorePermit,
    root: std::path::PathBuf,
    paths: shared::vfs::GamePaths,
    vfs: Arc<shared::vfs::VirtualFS>,
    pub(super) host: Arc<migo_io::scheduler::IoScheduler>,
}
impl Fixture {
    pub(super) async fn new() -> Self {
        static FILE_TESTS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> =
            std::sync::OnceLock::new();
        let scope = FILE_TESTS
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1)))
            .clone()
            .acquire_owned()
            .await
            .unwrap();
        let root = std::env::temp_dir().join(format!(
            "migo-file-stream-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let paths = shared::vfs::GamePaths::new(root.join("files"), root.join("cache"), "audio", 1)
            .unwrap();
        paths.ensure_directories().unwrap();
        fs::create_dir_all(paths.code_dir()).unwrap();
        let vfs = Arc::new(shared::vfs::VirtualFS::try_from_game_paths(&paths).unwrap());
        Self {
            _scope: scope,
            root,
            paths,
            vfs,
            host: Arc::new(migo_io::scheduler::IoScheduler::new(1001)),
        }
    }
    pub(super) async fn source(&self, data: &[u8]) -> Arc<dyn AudioFile> {
        fs::write(self.paths.user_data_dir().join("clip"), data).unwrap();
        migo_services::audio::LocalAudioFile::session(self.host.clone(), &self.paths)
            .source(self.vfs.clone(), "/user/clip".into())
            .await
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn wave(frames: u32) -> Vec<u8> {
    let bytes = frames * 2;
    let mut header = b"RIFF".to_vec();
    header.extend((bytes + 36).to_le_bytes());
    header.extend(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
    header.extend(48000_u32.to_le_bytes());
    header.extend(96000_u32.to_le_bytes());
    header.extend(b"\x02\0\x10\0data");
    header.extend(bytes.to_le_bytes());
    header
}
#[tokio::test]
async fn actual_large_file_src_plays_all_pcm_beyond_both_old_whole_file_limits() {
    let f = Fixture::new().await;
    let frames = crate::limits::MAX_AUDIO_PCM_SAMPLES as u32 + 129;
    let path = f.paths.user_data_dir().join("clip");
    {
        let mut file = std::io::BufWriter::with_capacity(65536, fs::File::create(&path).unwrap());
        file.write_all(&wave(frames)).unwrap();
        let body = [0, 32].repeat(4096);
        let mut left = frames as usize * 2;
        while left > 0 {
            let n = left.min(body.len());
            file.write_all(&body[..n]).unwrap();
            left -= n;
        }
        file.flush().unwrap();
    }
    let (tx, rx) = shared::audio_channel::channel();
    let sender = shared::op_state::AudioSender::new(tx, shared::channel::ThreadWakeup::new());
    let work = migo_services::audio::inner_audio_load_url(
        sender,
        migo_services::audio::InnerAudioSources {
            code_dir: None,
            vfs: Some(f.vfs.clone()),
            local_file: Some(migo_services::audio::LocalAudioFile::session(
                f.host.clone(),
                &f.paths,
            )),
            admit_remote: |_: &migo_services::network::url::Url| Ok(()),
        },
        91,
        101,
        "/user/clip".into(),
    )
    .unwrap();
    let mut player = InnerAudioPlayer::new(91, 1);
    player.shared.set_autoplay(true);
    let AudioCmd::InnerAudioBeginLoad {
        source_id, resp, ..
    } = rx.try_recv().unwrap()
    else {
        panic!("file begin")
    };
    resp.send(player.begin_load_for(source_id)).unwrap();
    let mut task = tokio::spawn(work);
    let until = Instant::now() + WAIT;
    let delivery = loop {
        if let Ok(value) = rx.try_recv() {
            break value;
        }
        if task.is_finished() {
            let result = (&mut task).await.unwrap();
            panic!("large local file was rejected before source handoff: {result:?}");
        }
        assert!(Instant::now() < until, "large file preparation stalled");
        tokio::task::yield_now().await;
    };
    let command = delivery;
    let AudioCmd::InnerAudioLoadFile {
        load, source, resp, ..
    } = command
    else {
        panic!("large file still entered whole encoded Vec/decode path")
    };
    assert!(source.length() > 16 * 1024 * 1024);
    assert_eq!(source.queued_bytes(), 128 * 1024);
    crate::audio_thread::start_inner_file_stream(
        &mut player,
        &f.host.audio_cpu(),
        48000,
        load,
        source,
        resp,
    );
    let mut checked = 0usize;
    let mut block = [0.; 8192];
    let mut paused = false;
    while checked < frames as usize {
        assert!(
            Instant::now() < until,
            "large local player made no progress"
        );
        player.poll_stream();
        assert!(player.queued_stream_messages() <= 4);
        if !paused && checked > 0 {
            player.pause();
            let position = player.shared.position_frames();
            let mut marker = [0.125; 17];
            player.process(&mut marker);
            assert_eq!(marker, [0.125; 17]);
            assert_eq!(player.shared.position_frames(), position);
            player.play();
            paused = true;
        }
        block.fill(0.);
        let before = player.shared.position_frames();
        let active = player.process(&mut block);
        let count = if active {
            player.shared.position_frames().saturating_sub(before) as usize
        } else if player.shared.state() == PlaybackState::Stopped && player.shared.is_loaded() {
            frames as usize - checked
        } else {
            0
        };
        assert!(count <= block.len());
        assert!(
            block[..count]
                .iter()
                .all(|s| s.to_bits() == 0.25_f32.to_bits()),
            "large file actual player PCM changed"
        );
        assert!(block[count..].iter().all(|s| *s == 0.));
        checked += count;
        assert!(
            player.physical_pcm_bytes() < 4 * 1024 * 1024,
            "large file retained track-sized PCM"
        );
        tokio::task::yield_now().await;
    }
    assert_eq!(checked, frames as usize);
    task.await.unwrap().unwrap();
    while !player.is_stream_complete() {
        assert!(
            Instant::now() < until,
            "large file never published final EOF"
        );
        player.poll_stream();
        tokio::task::yield_now().await;
    }
    assert!(player.is_stream_complete());
    assert!(
        player.retained_stream_start() > 0,
        "large file did not reclaim its consumed window"
    );
    println!(
        "MIGO_LARGE_FILE_PCM frames={checked} bits={} bounded_pcm_bytes={}",
        0.25_f32.to_bits(),
        player.physical_pcm_bytes()
    );
}
#[tokio::test]
async fn original_cpu_really_frees_the_file_backing_only_after_its_loan_step_returns() {
    let mut f = Fixture::new().await;
    f.host = Arc::new(
        migo_io::scheduler::IoScheduler::isolated_for_admission_probe(1002, 3, 128 * 1024),
    );
    let source = f.source(&vec![23; 131089]).await;
    let cancel = Arc::new(AtomicBool::new(false));
    let loan = source
        .clone()
        .read_at(0, 65536, cancel, Instant::now() + WAIT)
        .unwrap()
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let watch = migo_alloc_probe::DeallocationWatch::new(loan.as_ref().as_ptr()).unwrap();
    drop(source);
    let (started, waiting) = std::sync::mpsc::channel();
    let (release, held) = std::sync::mpsc::channel();
    let cpu = f.host.audio_cpu();
    let task = tokio::spawn(async move {
        cpu.run(move || {
            started.send(()).unwrap();
            held.recv_timeout(WAIT).unwrap();
            let before = migo_alloc_probe::thread_counts();
            drop(loan);
            let after = migo_alloc_probe::thread_counts();
            assert!(
                after.bytes_freed - before.bytes_freed >= 65536,
                "original CPU did not physically free the file backing"
            );
        })
        .await
        .unwrap();
    });
    tokio::task::spawn_blocking(move || waiting.recv_timeout(WAIT).unwrap())
        .await
        .unwrap();
    assert!(
        watch.freed_bytes().is_none(),
        "file input freed before original CPU returned"
    );
    assert!(f.host.pools().reserve_bytes(128 * 1024).is_err());
    assert_eq!(f.host.audio_cpu().run(|| 17).await.unwrap(), 17);
    release.send(()).unwrap();
    task.await.unwrap();
    assert_eq!(
        watch.freed_bytes(),
        Some(65536),
        "original file loan allocation was not actually destroyed"
    );
    assert!(f.host.pools().reserve_bytes(128 * 1024).is_ok());
}
#[tokio::test]
async fn native_file_readiness_keeps_typed_errors_and_source_refusal_without_duplicate_events() {
    let f = Fixture::new().await;
    for closed in [false, true] {
        let source = f.source(b"not an audio source").await;
        if closed {
            f.host.close();
        }
        let mut player = InnerAudioPlayer::new(91, 1);
        let load = player.begin_load_for(101).unwrap();
        let (reply, mut answer) = tokio::sync::oneshot::channel();
        crate::audio_thread::start_inner_file_stream(
            &mut player,
            &f.host.audio_cpu(),
            48000,
            load,
            source,
            reply,
        );
        let end = Instant::now() + WAIT;
        let error = loop {
            player.poll_stream();
            if let Ok(result) = answer.try_recv() {
                break result.unwrap_err();
            }
            assert!(Instant::now() < end);
            tokio::task::yield_now().await;
        };
        assert_eq!(
            error.code,
            if closed {
                ErrorCode::IoError
            } else {
                ErrorCode::InvalidArgument
            },
            "local file loading failure lost its typed cause"
        );
        assert!(
            !player
                .drain_events()
                .any(|e| e.event_type == InnerAudioEventType::Error),
            "file loading failure emitted duplicate native and RPC errors"
        );
    }
    drop(f);
    let f = Fixture::new().await;
    let source = f.source(b"not an audio source").await;
    let mut player = InnerAudioPlayer::new(91, 1);
    let old = player.begin_load_for(101).unwrap();
    player.begin_load_for(102).unwrap();
    let (reply, answer) = tokio::sync::oneshot::channel();
    crate::audio_thread::start_inner_file_stream(
        &mut player,
        &f.host.audio_cpu(),
        48000,
        old,
        source,
        reply,
    );
    assert_eq!(
        answer.await.unwrap().unwrap_err().code,
        ErrorCode::Cancelled
    );
    let source = f.source(b"\0\0\0\x18ftypM4A \0\0\0\0").await;
    let load = player.begin_load_for(103).unwrap();
    let (reply, answer) = tokio::sync::oneshot::channel();
    crate::audio_thread::start_inner_file_stream(
        &mut player,
        &f.host.audio_cpu(),
        48000,
        load,
        source,
        reply,
    );
    let mut answer = answer;
    let end = Instant::now() + WAIT;
    let error = loop {
        player.poll_stream();
        if let Ok(result) = answer.try_recv() {
            break result.unwrap_err();
        }
        assert!(Instant::now() < end);
        tokio::task::yield_now().await;
    };
    assert_eq!(
        error.code,
        ErrorCode::InvalidArgument,
        "malformed M4A lost its typed format error"
    );
    for rate in [16000, 48000] {
        let mut wav = wave(48001);
        wav.extend([0, 32].repeat(48001));
        let source = f.source(&wav).await;
        let load = player.begin_load_for(104 + rate).unwrap();
        let (reply, mut answer) = tokio::sync::oneshot::channel();
        crate::audio_thread::start_inner_file_stream(
            &mut player,
            &f.host.audio_cpu(),
            rate,
            load,
            source,
            reply,
        );
        assert!(
            matches!(
                answer.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ),
            "local file replied before native CanPlay"
        );
        assert!(
            player.loading_url().is_none(),
            "local pathname entered HTTP cache identity"
        );
        let end = Instant::now() + WAIT;
        loop {
            player.poll_stream();
            if let Ok(result) = answer.try_recv() {
                result.unwrap();
                break;
            }
            assert!(Instant::now() < end);
            tokio::task::yield_now().await;
        }
        assert_eq!(
            player.shared.sample_rate(),
            rate,
            "actor lost the actual output rate"
        );
        assert!(player.shared.is_loaded());
        player.stop();
    }
}

#[path = "streaming_file_index_tests.rs"]
mod indexed;

#[path = "streaming_file_control_tests.rs"]
pub(crate) mod controls;

#[path = "streaming_file_wave_tests.rs"]
mod wave_controls;

#[path = "streaming_file_mp3_tests.rs"]
mod mp3_controls;

#[path = "streaming_file_vorbis_tests.rs"]
mod vorbis_controls;
