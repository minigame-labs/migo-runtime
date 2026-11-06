//! Actual sealed File producer and public player controls; no manual ACKs.
use super::*;

pub(crate) fn deterministic() -> Vec<(&'static str, &'static [u8])> {
    vec![
        (
            "seek-front",
            include_bytes!("decoder/aac-fixtures/1-seek-front.m4a"),
        ),
        (
            "seek-tail",
            include_bytes!("decoder/aac-fixtures/1-seek-tail.m4a"),
        ),
        (
            "seek-co64",
            include_bytes!("decoder/aac-fixtures/2-seek-co64.m4a"),
        ),
        (
            "seek-reorder",
            include_bytes!("decoder/aac-fixtures/2-seek-reorder.m4a"),
        ),
        (
            "seek-edit",
            include_bytes!("decoder/aac-fixtures/1-seek-edit.m4a"),
        ),
    ]
}
pub(crate) async fn wait(
    player: &mut InnerAudioPlayer,
    mut done: impl FnMut(&InnerAudioPlayer) -> bool,
) {
    let end = Instant::now() + WAIT;
    loop {
        player.poll_stream();
        assert!(
            !player.drain_events().any(|e| {
                assert_eq!(e.source_id, 8181, "controlled source identity changed");
                e.event_type == InnerAudioEventType::Error
            }),
            "actual controlled file emitted Error"
        );
        if done(player) {
            return;
        }
        assert!(
            Instant::now() < end,
            "actual controlled file player stalled"
        );
        tokio::task::yield_now().await;
    }
}
pub(super) async fn player(f: &Fixture, data: &[u8], rate: u32) -> InnerAudioPlayer {
    let source = f.source(data).await;
    let mut player = InnerAudioPlayer::new(919, crate::decoder::decode(data).unwrap().channels);
    let load = player.begin_load_for(8181).unwrap();
    let (reply, result) = tokio::sync::oneshot::channel();
    crate::audio_thread::start_inner_file_stream(
        &mut player,
        &f.host.audio_cpu(),
        rate,
        load,
        source,
        reply,
    );
    wait(&mut player, |p| p.shared.is_loaded()).await;
    result.await.unwrap().unwrap();
    player
}
pub(crate) async fn suffix(player: &mut InnerAudioPlayer, frames: usize) -> Vec<f32> {
    let channels = player.shared.channels() as usize;
    let mut result = Vec::new();
    let mut out = vec![0.; 127 * channels];
    let end = Instant::now() + WAIT;
    player.play();
    while result.len() < frames * channels {
        player.poll_stream();
        assert!(
            !player.drain_events().any(|e| {
                assert_eq!(e.source_id, 8181, "controlled source identity changed");
                e.event_type == InnerAudioEventType::Error
            }),
            "controlled file failed while publishing complete suffix"
        );
        let before = player.shared.position_frames();
        out.fill(0.);
        let offered = (frames * channels - result.len()).min(out.len());
        let active = player.process(&mut out[..offered]);
        let n = if active {
            player.shared.position_frames().saturating_sub(before) as usize * channels
        } else if player.shared.state() == PlaybackState::Stopped && player.shared.is_loaded() {
            frames * channels - result.len()
        } else {
            0
        };
        assert!(
            n <= offered,
            "controlled source ended before its complete suffix"
        );
        result.extend_from_slice(&out[..n]);
        assert!(
            out[n..].iter().all(|x| *x == 0.),
            "controlled source published PCM outside its reported frame extent"
        );
        assert!(
            player.physical_pcm_bytes() < 4 * 1024 * 1024,
            "controlled file retained unbounded PCM"
        );
        assert!(Instant::now() < end, "controlled file suffix stalled");
        tokio::task::yield_now().await;
    }
    result
}
fn compare(got: &[f32], want: &[f32], key: &str) {
    assert_eq!(
        got.len(),
        want.len(),
        "controlled file complete suffix length: {key}"
    );
    let max = got
        .iter()
        .zip(want)
        .map(|(a, z)| (*a - *z).abs())
        .fold(0., f32::max);
    assert!(
        max <= 0.00001 && got.iter().chain(want).all(|x| x.is_finite()),
        "controlled file complete suffix PCM: {key}, error={max}"
    );
    if std::env::var_os("MIGO_DUMP_FILE_CONTROL_PCM").is_some() {
        println!(
            "MIGO_FILE_CONTROL_PCM {key} {:?}",
            got.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
        );
    }
}
#[tokio::test]
async fn public_file_seek_restart_and_exact_endpoint_preserve_complete_pcm() {
    let f = Fixture::new().await;
    for (name, data) in deterministic() {
        for rate in [16000, 48000] {
            let reference = crate::decoder::decode_for_rate(data, rate).unwrap();
            let mut p = player(&f, data, rate).await;

            let native = crate::decoder::decode(data).unwrap();
            let frames = native.samples.len() / native.channels as usize;
            for target in [frames / 2, frames - 1, frames, 0] {
                p.pause();
                let old_position = p.shared.position_frames();
                let seconds = if target == frames {
                    p.shared.duration()
                } else {
                    (target as f64 + 0.25) / native.sample_rate as f64
                };
                let mapped =
                    ((target as u128 * rate as u128).div_ceil(native.sample_rate as u128)) as usize;
                p.seek(seconds);
                assert_eq!(
                    p.shared.position_frames(),
                    old_position,
                    "file seek published before original source ACK"
                );
                wait(&mut p, |p| {
                    p.retained_stream_start() == mapped as u64
                        && p.shared.position_frames() == mapped as u64
                })
                .await;
                let n = reference.samples.len() / reference.channels as usize - mapped;
                let got = suffix(&mut p, n).await;
                if n == 0 {
                    wait(&mut p, |p| p.is_stream_complete()).await;
                }
                compare(
                    &got,
                    &reference.samples[mapped * reference.channels as usize..],
                    &format!("{name}/{rate}/seek-{target}"),
                );
                p.stop();
                p.play();
                wait(&mut p, |p| {
                    p.retained_stream_start() == 0 && p.shared.position_frames() == 0
                })
                .await;
                let got = suffix(
                    &mut p,
                    reference.samples.len() / reference.channels as usize,
                )
                .await;
                compare(
                    &got,
                    &reference.samples,
                    &format!("{name}/{rate}/restart-{target}"),
                );
            }
            drop(p);
        }
    }
}

pub(crate) async fn finish(p: &mut InnerAudioPlayer) {
    let mut ended = 0;
    let end = Instant::now() + WAIT;
    let mut out = vec![0.; 127 * p.shared.channels() as usize];
    loop {
        for e in p.drain_events() {
            assert_ne!(
                e.event_type,
                InnerAudioEventType::Error,
                "controlled file terminal Error"
            );
            ended += usize::from(e.event_type == InnerAudioEventType::Ended);
        }
        if p.shared.state() == PlaybackState::Stopped && p.is_stream_complete() {
            break;
        }
        p.poll_stream();
        out.fill(0.);
        p.process(&mut out);
        assert!(
            out.iter().all(|s| *s == 0.),
            "controlled source published PCM past its exact endpoint"
        );
        assert!(
            Instant::now() < end,
            "controlled source did not deliver final Ended"
        );
        tokio::task::yield_now().await;
    }
    ended += p
        .drain_events()
        .filter(|e| e.event_type == InnerAudioEventType::Ended)
        .count();
    assert_eq!(ended, 1, "controlled source duplicated or omitted Ended");
}
#[tokio::test]
async fn public_loop_cutovers_keep_the_global_sinc_grid_and_finish_the_consumed_cycle() {
    let f = Fixture::new().await;
    for (name, data) in deterministic()
        .into_iter()
        .filter(|(n, _)| matches!(*n, "seek-front" | "seek-co64" | "seek-edit"))
    {
        for rate in [3000, 16000, 48000] {
            let native = crate::decoder::decode(data).unwrap();
            let frames = native.samples.len() / native.channels as usize;
            let full = |cycles| {
                crate::resample_job::tests::reference(
                    &crate::decoder::DecodedAudio {
                        samples: native.samples.repeat(cycles),
                        sample_rate: native.sample_rate,
                        channels: native.channels,
                    },
                    rate,
                )
            };
            let mut p = player(&f, data, rate).await;
            p.set_loop_enabled(true);
            wait(&mut p, |p| p.shared.loop_enabled()).await;
            let prefix =
                ((frames as u128 * 2 * rate as u128) / (native.sample_rate as u128)) as usize + 3;
            let got = suffix(&mut p, prefix).await;
            compare(
                &got,
                &full(4)[..got.len()],
                &format!("{name}/{rate}/loop-prefix"),
            );
            assert!(p.shared.current_time() < p.shared.duration());
            p.pause();
            let at = p.shared.position_frames();
            p.set_loop_enabled(false);
            wait(&mut p, |p| {
                !p.shared.loop_enabled() && p.retained_stream_start() == at
            })
            .await;
            assert_eq!(
                p.shared.position_frames(),
                at,
                "loop disable moved the consumer boundary"
            );
            let cycle =
                ((at as u128 * native.sample_rate as u128) / (rate as u128)) / (frames as u128) + 1;
            let reference = full(cycle as usize);
            let tail = reference.len() / native.channels as usize - at as usize;
            let got = suffix(&mut p, tail).await;
            compare(
                &got,
                &reference[at as usize * native.channels as usize..],
                &format!("{name}/{rate}/loop-disabled-tail"),
            );
            finish(&mut p).await;
            p.set_loop_enabled(true);
            p.play();
            wait(&mut p, |p| {
                p.shared.loop_enabled() && p.retained_stream_start() == 0
            })
            .await;
            let got = suffix(&mut p, 17).await;
            compare(
                &got,
                &full(2)[..got.len()],
                &format!("{name}/{rate}/loop-restart"),
            );
        }
    }
}

// Independent mature pull demuxer/codec. Seeking PNS audio does not preserve
// the continuous decoder's random sequence; the codec's real seek is the oracle.
pub(crate) fn pull_seek(data: &[u8], origin: u64) -> crate::decoder::DecodedAudio {
    use symphonia_core::{
        codecs::audio::{AudioDecoder, AudioDecoderOptions},
        formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType},
        io::MediaSourceStream,
        units::Timestamp,
    };
    let stream = MediaSourceStream::new(Box::new(std::io::Cursor::new(data)), Default::default());
    let mut reader =
        symphonia_format_isomp4::IsoMp4Reader::try_new(stream, FormatOptions::default()).unwrap();
    let track = reader.default_track(TrackType::Audio).unwrap();
    let id = track.id;
    let params = track.codec_params.as_ref().unwrap().audio().unwrap();
    let mut codec = symphonia_codec_aac::AacDecoder::try_new(
        params,
        &AudioDecoderOptions::default().gapless(true),
    )
    .unwrap();
    reader
        .seek(
            SeekMode::Coarse,
            SeekTo::Timestamp {
                ts: Timestamp::new(origin as i64),
                track_id: id,
            },
        )
        .unwrap();
    let mut result = crate::decoder::DecodedAudio {
        samples: Vec::new(),
        sample_rate: 0,
        channels: 0,
    };
    let mut target = Some(origin as i64);
    while let Some(packet) = reader.next_packet().unwrap() {
        if packet.track_id != id {
            continue;
        }
        let block = codec.decode(&packet).unwrap();
        let skip = target.map_or(0, |ts| {
            (ts.saturating_sub(packet.pts.get() + packet.trim_start.get() as i64)
                .max(0) as usize)
                .min(block.frames())
        });
        if skip < block.frames() {
            target = None;
        }
        let block = block.slice(skip..);
        result.sample_rate = block.spec().rate();
        result.channels = block.spec().channels().count() as u32;
        let start = result.samples.len();
        result
            .samples
            .resize(start + block.samples_interleaved(), 0.);
        block.copy_to_slice_interleaved(&mut result.samples[start..]);
    }
    result
}
// For these AAC-LC mono/stereo formats every mature input quantum is128 frames.
// Recompute all cascade lattices and conservative history independently.
fn origin(source: u32, target: u32, frame: u64) -> u64 {
    if source == target {
        return frame;
    }
    let mut rate = source;
    let mut history = 0u64;
    let mut gcd = source;
    loop {
        history += (644u64 * u64::from(source)).div_ceil(u64::from(rate));
        let next = if u64::from(rate) > 4 * u64::from(target) {
            rate.div_ceil(4)
        } else {
            target
        };
        let (mut a, mut b) = (gcd, next);
        while b != 0 {
            (a, b) = (b, a % b);
        }
        gcd = a;
        if next == target {
            break;
        }
        rate = next;
    }
    let period = u64::from(source / gcd);
    frame.saturating_sub(history) / period * period
}
pub(crate) fn seek_reference(
    data: &[u8],
    rate: u32,
    start: u64,
    cycles: u64,
    input_target: Option<u64>,
) -> Vec<f32> {
    let native = pull_seek(data, 0);
    let n = native.samples.len() / native.channels as usize;
    let input = (u128::from(start) * u128::from(native.sample_rate) / u128::from(rate)) as u64;
    let base = origin(native.sample_rate, rate, input_target.unwrap_or(input));
    let mut suffix = pull_seek(data, base % n as u64);
    for _ in base / n as u64 + 1..cycles {
        suffix.samples.extend_from_slice(&native.samples);
    }
    let rendered = crate::resample_job::tests::reference(&suffix, rate);
    let first = (start
        - (u128::from(base) * u128::from(rate) / u128::from(native.sample_rate)) as u64)
        as usize
        * native.channels as usize;
    rendered[first..].to_vec()
}
#[tokio::test]
async fn pns_file_seeks_match_independent_mature_pull_seek_instead_of_continuous_noise() {
    let f = Fixture::new().await;
    for (name, data) in indexed::cases()
        .into_iter()
        .filter(|(n, _)| matches!(*n, "1-front" | "2-co64" | "edit-1537"))
    {
        for rate in [16000, 48000] {
            let native = pull_seek(data, 0);
            let frames = native.samples.len() / native.channels as usize;
            let target = frames / 2;
            let start = (target as u128 * rate as u128).div_ceil(native.sample_rate as u128) as u64;
            let reference = seek_reference(data, rate, start, 1, Some(target as u64));
            if name == "1-front" && rate == 16000 {
                let continuous = crate::decoder::decode_for_rate(data, rate).unwrap();
                let delta = reference
                    .iter()
                    .zip(&continuous.samples[start as usize * native.channels as usize..])
                    .map(|(a, b)| (*a - *b).abs())
                    .fold(0.0f32, f32::max);
                assert!(
                    delta > 0.00001,
                    "PNS oracle did not expose the real mature-seek noise discontinuity"
                );
            }
            let mut p = player(&f, data, rate).await;
            p.seek((target as f64 + 0.25) / native.sample_rate as f64);
            wait(&mut p, |p| {
                p.retained_stream_start() == start && p.shared.position_frames() == start
            })
            .await;
            let got = suffix(&mut p, reference.len() / native.channels as usize).await;
            compare(&got, &reference, &format!("pns-{name}/{rate}/seek"));
        }
    }
}

#[tokio::test]
async fn very_short_file_loops_publish_pcm_before_reposition_credit_is_exhausted() {
    let f = Fixture::new().await;
    let data = include_bytes!("decoder/aac-fixtures/0-front.m4a");
    let native = crate::decoder::decode(data).unwrap();
    assert_eq!(native.samples.len(), 17);
    for rate in [3000, 16000, 48000] {
        let mut p = player(&f, data, rate).await;
        p.set_loop_enabled(true);
        wait(&mut p, |p| p.shared.loop_enabled()).await;
        let got = suffix(&mut p, 137).await;
        let whole = crate::resample_job::tests::reference(
            &crate::decoder::DecodedAudio {
                samples: native.samples.repeat(3000),
                sample_rate: native.sample_rate,
                channels: 1,
            },
            rate,
        );
        compare(&got, &whole[..137], &format!("short/{rate}/loop"));
    }
}

async fn service_player(f: &Fixture, data: &[u8], rate: u32, packaged: bool) -> InnerAudioPlayer {
    let path = f.paths.user_data_dir().join("clip");
    indexed::padded(&path, data, 17 * 1024 * 1024);
    let mounts = if packaged {
        let package = f.root.join("control.mpkg");
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
    let (tx, rx) = shared::audio_channel::channel();
    let sender = shared::op_state::AudioSender::new(tx, shared::channel::ThreadWakeup::new());
    let work = migo_services::audio::inner_audio_load_url(
        sender,
        migo_services::audio::InnerAudioSources {
            code_dir: None,
            vfs: Some(f.vfs.clone()),
            local_file: Some(
                migo_services::audio::LocalAudioFile::session(f.host.clone(), &f.paths)
                    .with_mounts(mounts),
            ),
            admit_remote: |_: &migo_services::network::url::Url| Ok(()),
        },
        919,
        8181,
        if packaged { "/code/clip" } else { "/user/clip" }.into(),
    )
    .unwrap();
    let mut p = InnerAudioPlayer::new(919, crate::decoder::decode(data).unwrap().channels);
    let AudioCmd::InnerAudioBeginLoad {
        source_id, resp, ..
    } = rx.try_recv().unwrap()
    else {
        panic!("large control begin missing")
    };
    assert_eq!(source_id, 8181);
    resp.send(p.begin_load_for(source_id)).unwrap();
    let task = tokio::spawn(work);
    let deadline = Instant::now() + WAIT;
    let delivery = loop {
        if let Ok(d) = rx.try_recv_owned() {
            break d;
        }
        assert!(
            Instant::now() < deadline,
            "large control File handoff stalled"
        );
        tokio::task::yield_now().await;
    };
    let (cmd, permit) = delivery.into_parts();
    let AudioCmd::InnerAudioLoadFile {
        load, source, resp, ..
    } = cmd
    else {
        panic!("large controlled source bypassed File capability")
    };
    assert!(source.length() > 16 * 1024 * 1024);
    assert_eq!(load.source_id(), 8181);
    crate::audio_thread::start_inner_file_stream(
        &mut p,
        &f.host.audio_cpu(),
        rate,
        load,
        source,
        resp,
    );
    drop(permit);
    wait(&mut p, |p| p.shared.is_loaded()).await;
    task.await.unwrap().unwrap();
    p
}
#[tokio::test]
async fn large_file_and_package_public_controls_use_actual_service_file_handoff() {
    for packaged in [false, true] {
        for rate in [16000, 48000] {
            let f = Fixture::new().await;
            let data = include_bytes!("decoder/aac-fixtures/1-front.m4a");
            let native = pull_seek(data, 0);
            let n = native.samples.len() / native.channels as usize;
            let mut p = service_player(&f, data, rate, packaged).await;
            let target = n / 2;
            let start = (target as u128 * rate as u128).div_ceil(native.sample_rate as u128) as u64;
            p.seek((target as f64 + 0.25) / native.sample_rate as f64);
            wait(&mut p, |p| {
                p.retained_stream_start() == start && p.shared.position_frames() == start
            })
            .await;
            let reference = seek_reference(data, rate, start, 1, Some(target as u64));
            let got = suffix(&mut p, reference.len() / native.channels as usize).await;
            compare(&got, &reference, &format!("large-{packaged}/{rate}/seek"));
            p.stop();
            p.play();
            wait(&mut p, |p| {
                p.retained_stream_start() == 0 && p.shared.position_frames() == 0
            })
            .await;
            let reference = crate::resample_job::tests::reference(&native, rate);
            let got = suffix(&mut p, reference.len() / native.channels as usize).await;
            compare(
                &got,
                &reference,
                &format!("large-{packaged}/{rate}/restart"),
            );
            p.stop();
            p.set_loop_enabled(true);
            p.play();
            wait(&mut p, |p| {
                p.shared.loop_enabled() && p.retained_stream_start() == 0
            })
            .await;
            let count = reference.len() / native.channels as usize + 23;
            let got = suffix(&mut p, count).await;
            let repeated = crate::resample_job::tests::reference(
                &crate::decoder::DecodedAudio {
                    samples: native.samples.repeat(3),
                    sample_rate: native.sample_rate,
                    channels: native.channels,
                },
                rate,
            );
            compare(
                &got,
                &repeated[..got.len()],
                &format!("large-{packaged}/{rate}/loop"),
            );
            drop(p);
        }
    }
}

pub(super) struct Producer {
    pub(super) state: Arc<StreamingState>,
    pub(super) rx: tokio::sync::mpsc::Receiver<StreamMsg>,
    pub(super) errors: ErrorCell,
    pub(super) task: Option<tokio::task::JoinHandle<()>>,
}
impl Drop for Producer {
    fn drop(&mut self) {
        self.state.cancel();
    }
}
impl Producer {
    pub(super) fn new(f: &Fixture, file: Arc<dyn AudioFile>) -> Self {
        let state = StreamingState::new_reusable();
        let (tx, rx) = streaming::stream_channel();
        let errors = Arc::new(std::sync::Mutex::new(None));
        let future = run(
            f.host.audio_cpu(),
            file,
            tx,
            state.clone(),
            16000,
            errors.clone(),
        );
        Self {
            state,
            rx,
            errors,
            task: Some(tokio::spawn(future)),
        }
    }
    pub(super) async fn drain(&mut self, revision: streaming::StreamRevision) -> Vec<f32> {
        let mut pcm = Vec::new();
        loop {
            match tokio::time::timeout(WAIT, self.rx.recv())
                .await
                .unwrap()
                .expect("retained File producer closed")
            {
                StreamMsg::Ready { revision: r, .. } => assert_eq!(r, revision),
                StreamMsg::Samples {
                    revision: r,
                    pcm: p,
                } => {
                    assert_eq!(r, revision);
                    pcm.extend_from_slice(&p);
                }
                StreamMsg::Done { revision: r } => {
                    assert_eq!(r, revision);
                    break;
                }
                StreamMsg::Error { message, .. } => {
                    panic!("retained file revision error: {message}")
                }
            }
        }
        pcm
    }
    pub(super) async fn ended(&mut self) {
        tokio::time::timeout(WAIT, self.task.take().unwrap())
            .await
            .unwrap()
            .unwrap();
    }
}
#[tokio::test]
async fn refusal_and_source_ack_preserve_old_revision_until_the_consumer_accepts() {
    use crate::stream_control::{Reply, request};
    let f = Fixture::new().await;
    let data = include_bytes!("decoder/aac-fixtures/1-front.m4a");
    let path = f.paths.user_data_dir().join("clip");
    indexed::padded(&path, data, 128 * 1024);
    let file = migo_services::audio::LocalAudioFile::session(f.host.clone(), &f.paths)
        .source(f.vfs.clone(), "/user/clip".into())
        .await
        .unwrap();
    let weak = Arc::downgrade(&file);
    let mut producer = Producer::new(&f, file);
    let first = producer.drain(streaming::StreamRevision::INITIAL).await;
    assert_eq!(
        first,
        crate::decoder::decode_for_rate(data, 16000)
            .unwrap()
            .samples
    );
    assert!(
        !producer.task.as_ref().unwrap().is_finished(),
        "EOF discarded the original reusable producer"
    );
    for target in [-1, 100000] {
        let answer = request(
            producer.state.control_sender().unwrap(),
            target,
            tokio::time::Instant::now() + WAIT,
        )
        .unwrap()
        .await
        .unwrap();
        assert!(
            matches!(answer, Reply::Rejected(ErrorCode::InvalidArgument)),
            "invalid seek mutated source instead of refusing"
        );
        assert!(producer.rx.try_recv().is_err());
        assert!(!producer.state.is_cancelled());
    }
    let answer = request(
        producer.state.control_sender().unwrap(),
        211,
        tokio::time::Instant::now() - Duration::from_millis(1),
    )
    .unwrap()
    .await
    .unwrap();
    assert!(
        matches!(answer, Reply::NotStarted(ErrorCode::Timeout)),
        "expired request changed the live source"
    );
    let answer = request(
        producer.state.control_sender().unwrap(),
        4868,
        tokio::time::Instant::now() + WAIT,
    )
    .unwrap()
    .await
    .unwrap();
    let Reply::Prepared(prepared) = answer else {
        panic!("valid file seek did not prepare its original source")
    };
    let revision = prepared.revision;
    let start = prepared.mapped_output().unwrap();
    assert!(Arc::ptr_eq(&prepared.source, &producer.state));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), producer.rx.recv())
            .await
            .is_err(),
        "file published a new revision before consumer ACK"
    );
    prepared.accept.send(()).unwrap();
    let got = producer.drain(revision).await;
    compare(
        &got,
        &seek_reference(data, 16000, start, 1, Some(4868)),
        "ack/accepted-complete",
    );
    let answer = request(
        producer.state.control_sender().unwrap(),
        0,
        tokio::time::Instant::now() + WAIT,
    )
    .unwrap()
    .await
    .unwrap();
    let Reply::Prepared(prepared) = answer else {
        panic!("EOF restart lost retained index")
    };
    drop(prepared);
    producer.ended().await;
    assert!(
        producer.state.is_cancelled(),
        "dropped source receipt did not retire producer"
    );
    assert!(producer.rx.try_recv().is_err());
    assert!(producer.errors.lock().unwrap().is_none());
    let end = Instant::now() + WAIT;
    while weak.strong_count() != 0 {
        assert!(
            Instant::now() < end,
            "cancelled File owner retained after original CPU retirement"
        );
        tokio::task::yield_now().await;
    }
}

type ReadReply = tokio::sync::oneshot::Sender<
    std::thread::Result<EngineResult<shared::audio_file::AudioFileChunk>>,
>;
pub(super) struct Stalled {
    pub(super) file: Arc<dyn AudioFile>,
    pub(super) pause: AtomicBool,
    pub(super) revoked: AtomicBool,
    pub(super) changed: tokio::sync::Notify,
    pub(super) pending: std::sync::Mutex<Option<ReadReply>>,
}
impl AudioFile for Stalled {
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
        self.revoked.load(Ordering::Acquire) || self.file.is_closed()
    }
    fn closed_signal(&self) -> &tokio::sync::Notify {
        &self.changed
    }
    fn read_at(
        self: Arc<Self>,
        offset: u64,
        length: usize,
        cancelled: Arc<dyn shared::audio_file::AudioFileCancel>,
        deadline: Instant,
    ) -> Result<shared::audio_file::AudioFileRead, EngineError> {
        if self.pause.load(Ordering::Acquire) && length > 16 {
            let (tx, rx) = tokio::sync::oneshot::channel();
            *self.pending.lock().unwrap() = Some(tx);
            return Ok(rx);
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
async fn revocation_between_preparation_and_ack_keeps_the_old_revision() {
    let f = Fixture::new().await;
    let file = Arc::new(Stalled {
        file: f
            .source(include_bytes!("decoder/aac-fixtures/1-front.m4a"))
            .await,
        pause: AtomicBool::new(false),
        revoked: AtomicBool::new(false),
        changed: tokio::sync::Notify::new(),
        pending: std::sync::Mutex::new(None),
    });
    let mut p = Producer::new(&f, file.clone());
    p.drain(streaming::StreamRevision::INITIAL).await;
    let answer = crate::stream_control::request(
        p.state.control_sender().unwrap(),
        211,
        tokio::time::Instant::now() + WAIT,
    )
    .unwrap()
    .await
    .unwrap();
    let crate::stream_control::Reply::Prepared(prepared) = answer else {
        panic!("live file did not prepare seek before revocation")
    };
    file.revoked.store(true, Ordering::Release);
    file.changed.notify_waiters();
    prepared.accept.send(()).unwrap();
    assert!(
        matches!(
            tokio::time::timeout(WAIT, p.rx.recv()).await.unwrap(),
            Some(StreamMsg::Error {
                revision: streaming::StreamRevision::INITIAL,
                ..
            })
        ),
        "revoked File published a new revision after consumer ACK"
    );
    p.ended().await;
    assert_eq!(
        p.errors.lock().unwrap().as_ref().map(|e| e.code),
        Some(ErrorCode::IoError)
    );
    assert!(p.rx.try_recv().is_err());
}

#[tokio::test]
async fn one_absolute_control_deadline_covers_verified_media_read_before_ack() {
    let f = Fixture::new().await;
    let data = include_bytes!("decoder/aac-fixtures/1-front.m4a");
    let path = f.paths.user_data_dir().join("clip");
    indexed::padded(&path, data, 128 * 1024);
    let file = migo_services::audio::LocalAudioFile::session(f.host.clone(), &f.paths)
        .source(f.vfs.clone(), "/user/clip".into())
        .await
        .unwrap();
    let stalled = Arc::new(Stalled {
        file,
        pause: AtomicBool::new(false),
        revoked: AtomicBool::new(false),
        changed: tokio::sync::Notify::new(),
        pending: std::sync::Mutex::new(None),
    });
    let mut p = Producer::new(&f, stalled.clone());
    p.drain(streaming::StreamRevision::INITIAL).await;
    stalled.pause.store(true, Ordering::Release);
    let answer = crate::stream_control::request(
        p.state.control_sender().unwrap(),
        4868,
        tokio::time::Instant::now() + Duration::from_secs(1),
    )
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), p.rx.recv()).await;
    assert!(
        matches!(
            result,
            Ok(Some(StreamMsg::Error {
                revision: streaming::StreamRevision::INITIAL,
                ..
            }))
        ),
        "file control media read renewed or ignored its original deadline"
    );
    p.ended().await;
    assert_eq!(
        p.errors.lock().unwrap().as_ref().map(|e| e.code),
        Some(ErrorCode::Timeout)
    );
    assert!(
        answer.await.is_err(),
        "failed file source acknowledged a new revision"
    );
    assert!(
        stalled.pending.lock().unwrap().is_some(),
        "control never reached the actual blocked media read"
    );
    assert!(p.rx.try_recv().is_err());
}

#[tokio::test]
async fn loading_intents_and_ordered_live_controls_finish_on_the_last_accepted_mode() {
    let f = Fixture::new().await;
    let data = include_bytes!("decoder/aac-fixtures/1-seek-front.m4a");
    let mut p = InnerAudioPlayer::new(919, 2);
    let load = p.begin_load_for(8181).unwrap();
    let (reply, answer) = tokio::sync::oneshot::channel();
    crate::audio_thread::start_inner_file_stream(
        &mut p,
        &f.host.audio_cpu(),
        48000,
        load,
        f.source(data).await,
        reply,
    );
    p.set_start_time(0.1);
    p.stop();
    p.play();
    p.set_loop_enabled(true);
    p.seek(0.);
    p.set_loop_enabled(false);
    wait(&mut p, |p| p.shared.is_loaded()).await;
    answer.await.unwrap().unwrap();
    let expected = crate::decoder::decode_for_rate(data, 48000).unwrap();
    let got = suffix(&mut p, expected.samples.len() / 2).await;
    compare(
        &got,
        &expected.samples,
        "loading/explicit-zero-and-last-mode",
    );
    p.set_start_time(0.);
    p.pause();
    p.set_loop_enabled(true);
    p.seek(0.1);
    p.set_loop_enabled(false);
    p.stop();
    p.play();
    assert_eq!(
        p.shared.position_frames(),
        0,
        "queued stop exposed an older source seek"
    );
    wait(&mut p, |p| {
        !p.shared.loop_enabled()
            && p.retained_stream_start() == 0
            && p.shared.state() == PlaybackState::Playing
    })
    .await;
    let got = suffix(&mut p, expected.samples.len() / 2).await;
    compare(
        &got,
        &expected.samples,
        "ordered/loop-seek-disable-stop-play",
    );
    finish(&mut p).await;
}

#[tokio::test]
async fn cached_loops_revalidate_the_original_file_capability_without_new_io() {
    let f = Fixture::new().await;
    let data = include_bytes!("decoder/aac-fixtures/0-front.m4a");
    let file = Arc::new(Stalled {
        file: f.source(data).await,
        pause: AtomicBool::new(false),
        revoked: AtomicBool::new(false),
        changed: tokio::sync::Notify::new(),
        pending: std::sync::Mutex::new(None),
    });
    let mut p = InnerAudioPlayer::new(919, 1);
    let load = p.begin_load_for(8181).unwrap();
    let (reply, answer) = tokio::sync::oneshot::channel();
    crate::audio_thread::start_inner_file_stream(
        &mut p,
        &f.host.audio_cpu(),
        3000,
        load,
        file.clone(),
        reply,
    );
    wait(&mut p, |p| p.shared.is_loaded()).await;
    answer.await.unwrap().unwrap();
    p.set_loop_enabled(true);
    wait(&mut p, |p| p.shared.loop_enabled()).await;
    suffix(&mut p, 137).await;
    file.revoked.store(true, Ordering::Release);
    file.changed.notify_waiters();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut out = [0.; 127];
    let mut error = false;
    loop {
        p.poll_stream();
        for e in p.drain_events() {
            error |= e.event_type == InnerAudioEventType::Error;
        }
        if error {
            break;
        }
        out.fill(0.);
        p.process(&mut out);
        assert!(
            Instant::now() < deadline,
            "cached loop ignored revoked original source capability"
        );
        tokio::task::yield_now().await;
    }
    assert!(!p.shared.is_loaded());
    assert_eq!(p.shared.state(), PlaybackState::Stopped);
}
