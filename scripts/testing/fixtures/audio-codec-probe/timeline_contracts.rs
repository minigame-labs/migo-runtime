//! Real pinned file/HTTP and library presentation clocks, including AAC trims.
use super::*;
use crate::{
    candidate,
    inner_audio::InnerAudioPlayer,
    isomp4_file::IndexedFile,
    isomp4_source_contracts::{Config, client, fixture, start_server},
    isomp4_transport::IndexedHttp,
    streaming::{StreamMsg, StreamRevision, StreamingState, stream_channel},
    streaming_transport::{self, HttpSource, Source},
};
use std::time::Duration;
use symphonia_format_isomp4::streaming::{AtomKind, IndexOptions};
fn index(data: &[u8]) -> AudioIndex {
    let mut at = 0;
    loop {
        let span = AtomSpan::read(&data[at..], at as u64, Some(data.len() as u64))
            .unwrap()
            .unwrap();
        if span.kind == AtomKind::Movie {
            return AudioIndex::from_moov(
                &data[at..span.end.unwrap() as usize],
                IndexOptions {
                    object_len: Some(data.len() as u64),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        at = span.end.unwrap() as usize;
    }
}
#[test]
fn library_presentation_frames_include_trims_and_scaled_media_clocks_once() {
    let manifest: Vec<serde_json::Value> =
        serde_json::from_slice(&fixture("seek-manifest.json")).unwrap();
    for entry in &manifest {
        let data = fixture(entry["file"].as_str().unwrap());
        let idx = index(&data);
        let full = candidate::decode(&data, 16 * 1024 * 1024).unwrap();
        assert_eq!(
            idx.track().num_frames.unwrap(),
            (full.samples.len() / full.channels as usize) as u64
        );
        for output in [44100, 48000] {
            let timeline = from_track(idx.track(), output).unwrap();
            assert_eq!(
                timeline.seconds(),
                full.samples.len() as f64 / full.channels as f64 / full.sample_rate as f64
            );
            assert_eq!(
                timeline
                    .position(1000.min(idx.track().num_frames.unwrap()))
                    .unwrap()
                    .divisor,
                full.sample_rate
            );
            assert_eq!(timeline.format(), (output, full.channels));
        }
    }
    println!(
        "W09_TIMELINE {} library-trimmed track fixtures;28 clocks",
        manifest.len()
    );
}
#[test]
fn missing_or_incoherent_library_clocks_are_rejected() {
    let idx = index(&fixture("1-seek-front.m4a"));
    let base = idx.track();
    let mut missing = base.clone();
    missing.num_frames = None;
    assert!(from_track(&missing, 48000).is_err());
    let mut scaled = base.clone();
    scaled.time_base = Some(TimeBase::from_recip(NonZeroU32::new(88200).unwrap()));
    assert!(from_track(&scaled, 48000).is_err());
    let mut wrong = base.clone();
    wrong.num_frames = Some(base.num_frames.unwrap() + 1);
    assert!(from_track(&wrong, 48000).is_err());
    assert!(from_track(base, 0).is_err());
}
async fn consume<S: Source<Timed<B>> + 'static, const B: bool>(
    owner: migo_io::scheduler::IoScheduler,
    source: S,
    data: &[u8],
    output: u32,
) {
    let full = candidate::decode(data, 16 * 1024 * 1024).unwrap();
    let expected = crate::decoder_seek_contracts::resample(
        &full.samples,
        full.sample_rate,
        output,
        full.channels,
    );
    let (tx, mut rx) = stream_channel();
    let state = StreamingState::new();
    let active = state.clone();
    let len = data.len() as u64;
    let cpu = owner.audio_cpu();
    let task = tokio::spawn(async move {
        let result =
            streaming_transport::run_source(cpu, source, Some(len), &tx, &active, move |len| {
                Timed::<B>::new(output, len)
            })
            .await;
        streaming_transport::publish_result(&tx, &active, result).await;
    });
    let mut actual = Vec::new();
    let mut ready = 0;
    loop {
        match tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap()
        {
            StreamMsg::Ready {
                revision,
                sample_rate,
                channels,
            } => {
                ready += 1;
                assert_eq!(revision, StreamRevision::INITIAL);
                assert_eq!((sample_rate, channels), (output, full.channels));
                let timeline = state.timeline().expect("source clock must precede Ready");
                assert_eq!(
                    timeline.output_frames(),
                    (expected.len() / full.channels as usize) as u64
                );
                assert_eq!(
                    timeline.seconds(),
                    full.samples.len() as f64 / full.channels as f64 / full.sample_rate as f64
                );
            }
            StreamMsg::Samples { pcm, .. } => {
                assert_eq!(ready, 1);
                actual.extend_from_slice(&pcm);
            }
            StreamMsg::Done { .. } => break,
            StreamMsg::Error { message, .. } => panic!("timed source failed: {message}"),
        }
    }
    assert_eq!(ready, 1);
    crate::decoder_seek_contracts::assert_pcm(&actual, &expected);
    task.await.unwrap();
}
#[tokio::test]
async fn real_file_and_http_publish_authoritative_clock_before_first_ready() {
    for name in [
        "1-seek-front.m4a",
        "1-seek-tail.m4a",
        "1-seek-scaled.m4a",
        "1-seek-edit.m4a",
    ] {
        let data = fixture(name);
        for output in [44100, 48000] {
            let owner = migo_io::scheduler::IoScheduler::new(0);
            let path =
                std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join(name);
            let source = IndexedFile::open(std::fs::File::open(path).unwrap(), &owner)
                .await
                .unwrap();
            consume::<_, true>(owner, source, &data, output).await;
            let server = start_server(data.clone(), Config::default()).await;
            let client = client();
            let response = <IndexedHttp as HttpSource<Timed<false>>>::request(client.get(
                crate::network::gate::GateKind::AudioStream,
                server.url.clone(),
            ))
            .send()
            .await
            .unwrap();
            let source = <IndexedHttp as HttpSource<Timed<false>>>::new(client, response).unwrap();
            consume::<_, false>(
                migo_io::scheduler::IoScheduler::new(0),
                source,
                &data,
                output,
            )
            .await;
        }
    }
    println!("W09_TIMED_PCM16 exact file/HTTP streams including scaled clocks and trims");
}
#[tokio::test]
async fn the_real_player_keeps_total_duration_during_prefetch_and_seek_wait() {
    let name = "2-seek-front.m4a";
    let data = fixture(name);
    let full = candidate::decode(&data, 16 * 1024 * 1024).unwrap();
    let owner = migo_io::scheduler::IoScheduler::new(0);
    let path = std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join(name);
    let source = IndexedFile::open(std::fs::File::open(path).unwrap(), &owner)
        .await
        .unwrap();
    let (tx, rx) = stream_channel();
    let state = StreamingState::new();
    let active = state.clone();
    let cpu = owner.audio_cpu();
    let len = data.len() as u64;
    let rate = full.sample_rate;
    let task = tokio::spawn(async move {
        let result =
            streaming_transport::run_source(cpu, source, Some(len), &tx, &active, move |len| {
                Timed::<true>::new(rate, len)
            })
            .await;
        streaming_transport::publish_result(&tx, &active, result).await;
    });
    let mut player = InnerAudioPlayer::new(900, full.channels);
    player
        .start_streaming(name.into(), rx, state.clone())
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !player.shared.is_loaded() {
            player.poll_stream();
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!player.is_stream_complete());
    assert_eq!(
        player.shared.duration_frames(),
        (full.samples.len() / full.channels as usize) as u64
    );
    assert_eq!(
        player.shared.duration(),
        full.samples.len() as f64 / full.channels as f64 / rate as f64
    );
    player.seek(2.0);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            player.poll_stream();
            if player
                .drain_events()
                .any(|e| e.event_type == shared::protocol::audio_cmd::InnerAudioEventType::Seeked)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(player.shared.position_frames(), 2 * rate as u64);
    assert_eq!(
        player.shared.duration_frames(),
        (full.samples.len() / full.channels as usize) as u64
    );
    state.cancel();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
}
