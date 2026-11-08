use super::*;
use crate::test_media::{AtomKind, AtomSpan};

async fn collect(f: &Fixture, source: Arc<dyn AudioFile>, rate: u32) -> (Vec<f32>, u64) {
    let state = StreamingState::new();
    let (tx, mut rx) = streaming::stream_channel();
    let errors = Arc::new(std::sync::Mutex::new(None));
    let _ = &f;
    crate::stream_producer::spawn(
        crate::stream_open::file(source),
        rate,
        tx,
        state.clone(),
        Some(errors.clone()),
    )
    .unwrap();
    let mut actual = Vec::new();
    let mut ready = 0;
    let mut done = 0;
    while let Some(msg) = tokio::time::timeout(WAIT, rx.recv()).await.unwrap() {
        match msg {
            StreamMsg::Ready { sample_rate, .. } => {
                assert_eq!(sample_rate, rate);
                ready += 1;
            }
            StreamMsg::Samples { pcm, .. } => actual.extend_from_slice(&pcm),
            StreamMsg::Done { .. } => done += 1,
            StreamMsg::Error { message, .. } => {
                panic!("indexed file failed complete PCM: {message}")
            }
        }
    }
    assert_eq!((ready, done), (1, 1));
    assert!(errors.lock().unwrap().is_none());
    (actual, state.bytes_downloaded.load(Ordering::Acquire))
}
pub(super) fn cases() -> Vec<(&'static str, &'static [u8])> {
    vec![
        (
            "0-front",
            include_bytes!("decoder/aac-fixtures/0-front.m4a"),
        ),
        ("0-tail", include_bytes!("decoder/aac-fixtures/0-tail.m4a")),
        (
            "1-front",
            include_bytes!("decoder/aac-fixtures/1-front.m4a"),
        ),
        ("1-tail", include_bytes!("decoder/aac-fixtures/1-tail.m4a")),
        (
            "1-reorder",
            include_bytes!("decoder/aac-fixtures/1-reorder.m4a"),
        ),
        (
            "1-split",
            include_bytes!("decoder/aac-fixtures/1-split.m4a"),
        ),
        ("1-co64", include_bytes!("decoder/aac-fixtures/1-co64.m4a")),
        (
            "2-front",
            include_bytes!("decoder/aac-fixtures/2-front.m4a"),
        ),
        ("2-tail", include_bytes!("decoder/aac-fixtures/2-tail.m4a")),
        (
            "2-reverse",
            include_bytes!("decoder/aac-fixtures/2-reverse.m4a"),
        ),
        (
            "2-reorder",
            include_bytes!("decoder/aac-fixtures/2-reorder.m4a"),
        ),
        ("2-co64", include_bytes!("decoder/aac-fixtures/2-co64.m4a")),
        (
            "2-tail-reverse",
            include_bytes!("decoder/aac-fixtures/2-tail-reverse.m4a"),
        ),
        (
            "2-multi",
            include_bytes!("decoder/aac-fixtures/2-multi.m4a"),
        ),
        (
            "2-tail-multi",
            include_bytes!("decoder/aac-fixtures/2-tail-multi.m4a"),
        ),
        (
            "2-split",
            include_bytes!("decoder/aac-fixtures/2-split.m4a"),
        ),
        (
            "edit-raw",
            include_bytes!("decoder/aac-fixtures/edit-raw.m4a"),
        ),
        (
            "edit-211",
            include_bytes!("decoder/aac-fixtures/edit-211.m4a"),
        ),
        (
            "edit-1537",
            include_bytes!("decoder/aac-fixtures/edit-1537.m4a"),
        ),
        (
            "edit-v1",
            include_bytes!("decoder/aac-fixtures/edit-version-one.m4a"),
        ),
    ]
}
#[tokio::test]
async fn indexed_file_full_pcm_preserves_front_tail_co64_reorder_and_edits() {
    let f = Fixture::new().await;
    for (name, data) in cases() {
        for rate in [16000, 48000] {
            let expected = crate::decoder::decode_for_rate(data, rate).unwrap();
            let source = f.source(data).await;
            let (actual, _) = collect(&f, source, rate).await;
            assert_eq!(
                actual.len(),
                expected.samples.len(),
                "indexed file complete frame count changed"
            );
            assert!(
                actual
                    .iter()
                    .zip(&expected.samples)
                    .all(|(a, z)| a.to_bits() == z.to_bits()),
                "indexed file complete PCM bits changed: {name}/{rate}"
            );
            if std::env::var_os("MIGO_DUMP_INDEXED_FILE_PCM").is_some() {
                println!(
                    "MIGO_INDEXED_FILE_PCM {name}/{rate} {:?}",
                    actual.iter().map(|s| s.to_bits()).collect::<Vec<_>>()
                );
            }
        }
    }
}
// Test-only file writer, not a product parser. Add a valid unused mdat prefix
// to the unchanged fixture and shift its library index offsets. No large Vec.
pub(super) fn padded(path: &std::path::Path, data: &[u8], gap: u32) {
    use std::io::{Seek, SeekFrom};
    fn offsets(data: &mut [u8], gap: u32) {
        let mut pos = 0;
        while pos < data.len() {
            let len = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
            assert!(len >= 8 && pos + len <= data.len());
            match &data[pos + 4..pos + 8] {
                b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" => {
                    offsets(&mut data[pos + 8..pos + len], gap)
                }
                b"stco" => {
                    let count =
                        u32::from_be_bytes(data[pos + 12..pos + 16].try_into().unwrap()) as usize;
                    for i in 0..count {
                        let a = pos + 16 + i * 4;
                        let n = u32::from_be_bytes(data[a..a + 4].try_into().unwrap());
                        data[a..a + 4].copy_from_slice(&(n + gap).to_be_bytes());
                    }
                }
                b"co64" => {
                    let count =
                        u32::from_be_bytes(data[pos + 12..pos + 16].try_into().unwrap()) as usize;
                    for i in 0..count {
                        let a = pos + 16 + i * 8;
                        let n = u64::from_be_bytes(data[a..a + 8].try_into().unwrap());
                        data[a..a + 8].copy_from_slice(&(n + u64::from(gap)).to_be_bytes());
                    }
                }
                _ => (),
            }
            pos += len;
        }
    }
    let mut file = fs::File::create(path).unwrap();
    let suffix = 64 * 1024;
    let mut pos = 0;
    while pos < data.len() {
        let span = AtomSpan::read(&data[pos..], pos as u64, Some(data.len() as u64))
            .unwrap()
            .unwrap();
        let end = span.end.unwrap() as usize;
        match span.kind {
            AtomKind::MediaData => {
                assert_eq!(span.header_len, 8);
                file.write_all(&((end - pos) as u32 + gap + suffix).to_be_bytes())
                    .unwrap();
                file.write_all(b"mdat").unwrap();
                file.seek(SeekFrom::Current(i64::from(gap))).unwrap();
                file.write_all(&data[pos + 8..end]).unwrap();
                file.seek(SeekFrom::Current(i64::from(suffix))).unwrap();
            }
            AtomKind::Movie => {
                let mut moov = data[pos..end].to_vec();
                offsets(&mut moov, gap);
                file.write_all(&moov).unwrap();
            }
            _ => file.write_all(&data[pos..end]).unwrap(),
        }
        pos = end;
    }
    let length = file.stream_position().unwrap();
    file.set_len(length).unwrap();
    file.flush().unwrap();
}
// Transparent observation of original media IO; headers and control reads do
// not count as repositioned media, and original loans/credits are unchanged.
struct Observed {
    file: Arc<dyn AudioFile>,
    end: std::sync::atomic::AtomicU64,
    backwards: std::sync::atomic::AtomicUsize,
}
impl AudioFile for Observed {
    fn read_blocking(&self, offset: u64, output: &mut [u8]) -> shared::error::EngineResult<()> {
        if output.len() > 16 {
            let previous = self
                .end
                .swap(offset + output.len() as u64, Ordering::AcqRel);
            if offset < previous {
                self.backwards.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.file.read_blocking(offset, output)
    }
    fn length(&self) -> u64 {
        self.file.length()
    }
    fn prefix(&self) -> &[u8] {
        self.file.prefix()
    }
    fn queued_bytes(&self) -> usize {
        self.file.queued_bytes()
    }
    fn is_closed(&self) -> bool {
        self.file.is_closed()
    }
    fn closed_signal(&self) -> &tokio::sync::Notify {
        self.file.closed_signal()
    }
    fn read_at(
        self: Arc<Self>,
        offset: u64,
        length: usize,
        cancelled: Arc<dyn shared::audio_file::AudioFileCancel>,
        deadline: Instant,
    ) -> Result<shared::audio_file::AudioFileRead, EngineError> {
        if length > 16 {
            let previous = self.end.swap(offset + length as u64, Ordering::AcqRel);
            if offset < previous {
                self.backwards.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.file
            .clone()
            .read_at(offset, length, cancelled, deadline)
    }
    fn read_control_at(
        self: Arc<Self>,
        offset: u64,
        length: usize,
        cancelled: Arc<dyn shared::audio_file::AudioFileCancel>,
        deadline: Instant,
    ) -> Result<shared::audio_file::AudioFileRead, EngineError> {
        self.file
            .clone()
            .read_control_at(offset, length, cancelled, deadline)
    }
}
#[tokio::test]
async fn actual_large_m4a_file_src_reaches_native_player_with_complete_pcm_and_identity() {
    large_product(false).await;
}
#[tokio::test]
async fn actual_large_package_m4a_src_reaches_native_player_with_complete_pcm_and_identity() {
    large_product(true).await;
}
async fn large_product(packaged: bool) {
    for (name, data) in cases().into_iter().filter(|(n, _)| {
        matches!(
            *n,
            "1-front" | "1-tail" | "2-reorder" | "2-tail-reverse" | "2-co64"
        )
    }) {
        for rate in [16000, 48000] {
            let f = Fixture::new().await;
            let path = f.paths.user_data_dir().join("clip");
            padded(&path, data, 17 * 1024 * 1024);
            let mounts = if packaged {
                let package = f.root.join("audio.mpkg");
                let mut writer =
                    shared::vfs::PackageWriter::new(fs::File::create(&package).unwrap()).unwrap();
                writer
                    .add_entry_streaming("clip", fs::File::open(&path).unwrap(), 64 * 1024 * 1024)
                    .unwrap();
                writer.finish("audio", "1").unwrap();
                let mounts = Arc::new(shared::vfs::MountTable::new(
                    f.paths.code_dir().to_path_buf(),
                ));
                mounts.swap_base(Arc::new(
                    shared::vfs::PackSource::open(&package, "audio", "1").unwrap(),
                ));
                Some(mounts)
            } else {
                None
            };
            let expected = crate::decoder::decode_for_rate(data, rate).unwrap();
            let (tx, rx) = shared::audio_channel::channel();
            let sender =
                shared::op_state::AudioSender::new(tx, shared::channel::ThreadWakeup::new());
            let work = migo_services::audio::inner_audio_load_url(
                sender,
                migo_services::audio::InnerAudioSources {
                    code_dir: None,
                    vfs: Some(f.vfs.clone()),
                    local_file: Some(
                        migo_services::audio::LocalAudioFile::session(f.host.clone(), &f.paths)
                            .with_mounts(mounts.clone()),
                    ),
                    admit_remote: |_: &migo_services::network::url::Url| Ok(()),
                },
                91,
                3901,
                if packaged { "/code/clip" } else { "/user/clip" }.into(),
            )
            .unwrap();
            let mut player = InnerAudioPlayer::new(91, expected.channels);
            player.shared.set_autoplay(true);
            let AudioCmd::InnerAudioBeginLoad {
                source_id, resp, ..
            } = rx.try_recv().unwrap()
            else {
                panic!("M4A begin missing")
            };
            assert_eq!(source_id, 3901);
            resp.send(player.begin_load_for(source_id)).unwrap();
            let mut task = tokio::spawn(work);
            let end = Instant::now() + WAIT;
            let delivery = loop {
                if let Ok(c) = rx.try_recv() {
                    break c;
                }
                if task.is_finished() {
                    panic!(
                        "large M4A refused before capability handoff: {:?}",
                        (&mut task).await.unwrap()
                    );
                }
                assert!(Instant::now() < end);
                tokio::task::yield_now().await;
            };
            let cmd = delivery;
            let AudioCmd::InnerAudioLoadFile {
                load, source, resp, ..
            } = cmd
            else {
                panic!("large M4A was materialized as whole encoded bytes")
            };
            assert_eq!(load.source_id(), 3901);
            assert!(source.length() > 16 * 1024 * 1024);
            let observed = Arc::new(Observed {
                file: source,
                end: std::sync::atomic::AtomicU64::new(0),
                backwards: std::sync::atomic::AtomicUsize::new(0),
            });
            let source: Arc<dyn AudioFile> = observed.clone();
            crate::audio_thread::start_inner_file_stream(
                &mut player,
                &f.host.audio_cpu(),
                rate,
                load,
                source,
                resp,
            );
            let mut actual = Vec::new();
            let mut output = vec![0.; (expected.channels as usize) * 257];
            loop {
                assert!(Instant::now() < end, "large M4A player stalled");
                player.poll_stream();
                if task.is_finished() && !player.shared.is_loaded() {
                    panic!(
                        "large M4A was refused by the actual native actor: {:?}",
                        (&mut task).await.unwrap()
                    );
                }
                let before = player.shared.position_frames();
                output.fill(0.);
                let active = player.process(&mut output);
                let n = if active {
                    player.shared.position_frames().saturating_sub(before) as usize
                        * expected.channels as usize
                } else if player.shared.state() == PlaybackState::Stopped
                    && player.shared.is_loaded()
                {
                    expected.samples.len() - actual.len()
                } else {
                    0
                };
                assert!(n <= output.len());
                actual.extend_from_slice(&output[..n]);
                assert!(output[n..].iter().all(|s| *s == 0.));
                if actual.len() == expected.samples.len() && player.is_stream_complete() {
                    break;
                }
                assert!(player.physical_pcm_bytes() < 4 * 1024 * 1024);
                tokio::task::yield_now().await;
            }
            task.await
                .unwrap()
                .expect("large M4A was refused by the actual native actor");
            if matches!(name, "2-reorder" | "2-tail-reverse") {
                assert!(
                    observed.backwards.load(Ordering::Acquire) > 0,
                    "large M4A never executed actual backward ReadAt"
                );
            }

            assert!(
                actual
                    .iter()
                    .zip(&expected.samples)
                    .all(|(a, z)| a.to_bits() == z.to_bits()),
                "actual large M4A player complete PCM changed: {name}/{rate}"
            );
            if std::env::var_os(if packaged {
                "MIGO_DUMP_PACKAGE_FILE_PCM"
            } else {
                "MIGO_DUMP_INDEXED_FILE_PCM"
            })
            .is_some()
            {
                println!(
                    "{} large-{name}/{rate} {:?}",
                    if packaged {
                        "MIGO_PACKAGE_INDEXED_PCM"
                    } else {
                        "MIGO_INDEXED_FILE_PCM"
                    },
                    actual.iter().map(|s| s.to_bits()).collect::<Vec<_>>()
                );
            }
        }
    }
}

#[tokio::test]
async fn original_cpu_really_destroys_the_exact_index_allocation_before_credit_returns() {
    let mut f = Fixture::new().await;
    let bytes = 2 * 1024 * 1024;
    f.host = Arc::new(
        migo_io::scheduler::IoScheduler::isolated_for_admission_probe(
            1011,
            3,
            (bytes + 128 * 1024) as u64,
        ),
    );
    let source = f.source(&vec![23; bytes + 1]).await;
    let loan = source
        .clone()
        .read_control_at(
            0,
            bytes,
            Arc::new(AtomicBool::new(false)),
            Instant::now() + WAIT,
        )
        .unwrap()
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(loan.as_ref().len(), bytes);
    assert!(loan.as_ref().iter().all(|v| *v == 23));
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
                after.bytes_freed - before.bytes_freed >= bytes as u64,
                "original CPU did not really free the index allocation"
            );
        })
        .await
        .unwrap();
    });
    tokio::task::spawn_blocking(move || waiting.recv_timeout(WAIT).unwrap())
        .await
        .unwrap();
    let charged = f.host.pools().reserve_bytes(bytes as u64).is_err();
    let allocated = watch.freed_bytes().is_none();
    assert_eq!(f.host.audio_cpu().run(|| 17).await.unwrap(), 17);
    release.send(()).unwrap();
    task.await.unwrap();
    assert!(
        allocated,
        "index input freed before original CPU completion"
    );
    assert!(
        charged,
        "index credit returned before original CPU destruction"
    );
    assert_eq!(watch.freed_bytes(), Some(bytes));
    assert!(
        f.host
            .pools()
            .reserve_bytes((bytes + 128 * 1024) as u64)
            .is_ok()
    );
}
