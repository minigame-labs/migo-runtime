//! Public pinned-Range controls use the original reader, CPU and PCM consumer.
use super::controls::{Context, player};
use super::*;
use crate::streaming_file::tests::controls::{
    deterministic, finish, pull_seek, seek_reference, suffix, wait,
};
use crate::streaming_transport::InterruptibleSource;
fn compare(got: &[f32], want: &[f32], key: &str) {
    assert_eq!(
        got.len(),
        want.len(),
        "HTTP controlled complete suffix length: {key}"
    );
    let max = got
        .iter()
        .zip(want)
        .map(|(a, z)| (*a - *z).abs())
        .fold(0., f32::max);
    assert!(
        max <= 0.00001 && got.iter().chain(want).all(|x| x.is_finite()),
        "HTTP controlled complete suffix PCM: {key},error={max}"
    );
    if std::env::var_os("MIGO_DUMP_RANGE_CONTROL_PCM").is_some() {
        println!(
            "MIGO_RANGE_CONTROL_PCM {key} {:?}",
            got.iter().map(|s| s.to_bits()).collect::<Vec<_>>()
        );
    }
}

#[tokio::test]
async fn public_http_seek_restart_and_exact_endpoint_preserve_complete_pcm() {
    for mode in [Mode::Normal] {
        let f = Context::new(mode);
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
                    let mapped = ((target as u128 * rate as u128)
                        .div_ceil(native.sample_rate as u128))
                        as usize;
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
                        &format!("{}/{name}/{rate}/seek-{target}", "range"),
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
                        &format!("{}/{name}/{rate}/restart-{target}", "range"),
                    );
                }
                drop(p);
            }
        }
    }
}

#[tokio::test]
async fn public_loop_cutovers_keep_the_global_sinc_grid_and_finish_the_consumed_cycle() {
    for mode in [Mode::Normal] {
        let f = Context::new(mode);
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
                let prefix = ((frames as u128 * 2 * rate as u128) / (native.sample_rate as u128))
                    as usize
                    + 3;
                let got = suffix(&mut p, prefix).await;
                compare(
                    &got,
                    &full(4)[..got.len()],
                    &format!("{}/{name}/{rate}/loop-prefix", "range"),
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
                let cycle = ((at as u128 * native.sample_rate as u128) / (rate as u128))
                    / (frames as u128)
                    + 1;
                let reference = full(cycle as usize);
                let tail = reference.len() / native.channels as usize - at as usize;
                let got = suffix(&mut p, tail).await;
                compare(
                    &got,
                    &reference[at as usize * native.channels as usize..],
                    &format!("{}/{name}/{rate}/loop-disabled-tail", "range"),
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
                    &format!("{}/{name}/{rate}/loop-restart", "range"),
                );
            }
        }
    }
}

#[tokio::test]
async fn pns_http_seeks_match_independent_mature_pull_seek_instead_of_continuous_noise() {
    for mode in [Mode::Normal] {
        let f = Context::new(mode);
        for (name, data) in cases()
            .into_iter()
            .filter(|(n, _)| matches!(*n, "1-front" | "2-co64" | "edit-1537"))
        {
            for rate in [16000, 48000] {
                let native = pull_seek(data, 0);
                let frames = native.samples.len() / native.channels as usize;
                let target = frames / 2;
                let start =
                    (target as u128 * rate as u128).div_ceil(native.sample_rate as u128) as u64;
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
                compare(
                    &got,
                    &reference,
                    &format!("{}/pns-{name}/{rate}/seek", "range"),
                );
            }
        }
    }
}

#[tokio::test]
async fn very_short_http_loops_publish_pcm_before_reposition_credit_is_exhausted() {
    for mode in [Mode::Normal] {
        let f = Context::new(mode);
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
            compare(
                &got,
                &whole[..137],
                &format!("{}/short/{rate}/loop", "range"),
            );
        }
    }
}

async fn reader_source(
    server: &Server,
    host: &migo_io::scheduler::IoScheduler,
    limits: range::Limits,
    length: u64,
) -> IndexedHttp {
    let client = server.client(host.pools().clone());
    let response = client
        .get(
            GateKind::AudioStream,
            migo_services::network::url::Url::parse(
                "http://indexed-product-fixture.invalid/source",
            )
            .unwrap(),
        )
        .header(
            "accept-encoding".parse().unwrap(),
            "identity".parse().unwrap(),
        )
        .send()
        .await
        .unwrap();
    let identity = range::Identity::capture(&response).unwrap();
    let mut source = IndexedHttp::new(client.clone(), response).unwrap();
    let reader = range::Reader::new(client, GateKind::AudioStream, identity, limits);
    source.initial = None;
    source.owner = Owner::Media(Box::new(
        reader
            .into_stream_at(0, length, range::StreamLimits::default())
            .unwrap(),
    ));
    source
}
#[tokio::test]
async fn interrupted_range_read_keeps_every_byte_and_original_reader_budget() {
    let data: Vec<u8> = (0..9737).map(|n| (n % 251) as u8).collect();
    let server = server(data.clone(), Mode::Normal, false, false).await;
    let host = migo_io::scheduler::IoScheduler::new(1087);
    let mut source = reader_source(
        &server,
        &host,
        range::Limits {
            max_requests: 1,
            ..Default::default()
        },
        data.len() as u64,
    )
    .await;
    // Spend the one control request before media, then carry its exact reader through interruption.
    let reader = source.control().unwrap();
    reader
        .read_exact_into(0, &mut [0; 8], Instant::now() + WAIT)
        .await
        .unwrap();
    assert_eq!(reader.requests(), 1);
    let Owner::Control(reader) = std::mem::replace(&mut source.owner, Owner::Failed) else {
        panic!()
    };
    source.owner = Owner::Media(Box::new(
        reader
            .into_stream_at(0, data.len() as u64, range::StreamLimits::default())
            .unwrap(),
    ));
    server.pause_range_body(2);
    let (tx, _rx) = crate::streaming::stream_channel();
    let state = StreamingState::new_url();
    let mut got = match source.read(&tx, &state).await.unwrap() {
        SourceRead::Data(b) => b.as_ref().to_vec(),
        _ => panic!(),
    };
    tokio::time::timeout(WAIT, server.entered.notified())
        .await
        .unwrap();
    let count = server.range_requests();
    {
        let read = source.read(&tx, &state);
        tokio::pin!(read);
        std::future::poll_fn(|cx| {
            assert!(
                std::future::Future::poll(read.as_mut(), cx).is_pending(),
                "fixture failed to interrupt the actual pending body"
            );
            std::task::Poll::Ready(())
        })
        .await;
    }
    source
        .resume_after_read_interruption()
        .expect("pinned HTTP did not recover its interrupted read");
    server.release.notify_one();
    while let SourceRead::Data(b) = source.read(&tx, &state).await.unwrap() {
        got.extend_from_slice(b.as_ref());
    }
    assert_eq!(
        got, data,
        "interrupted HTTP lost or repeated a payload byte"
    );
    assert_eq!(
        server.range_requests(),
        count,
        "interrupted HTTP replaced its already validated body"
    );
    let reader = source.control().unwrap();
    assert_eq!(
        (reader.requests(), reader.bytes_received()),
        (1, 8),
        "HTTP control reader budget was reset by interruption"
    );
    assert!(
        reader
            .read_exact_into(0, &mut [0; 1], Instant::now() + WAIT)
            .await
            .is_err(),
        "HTTP control reader budget was renewed"
    );
    assert_eq!(server.range_requests(), count);
}
#[tokio::test]
async fn returned_range_validation_and_body_errors_never_become_interruptions() {
    let host = migo_io::scheduler::IoScheduler::new(1088);
    for mode in [Mode::Changed, Mode::Offset, Mode::Truncated] {
        let server = server(vec![23; 9737], Mode::Normal, false, false).await;
        let mut source = reader_source(&server, &host, range::Limits::default(), 9737).await;
        server.range_fault(mode);
        let (tx, _rx) = crate::streaming::stream_channel();
        let state = StreamingState::new_url();
        loop {
            match source.read(&tx, &state).await {
                Ok(SourceRead::Data(_)) => {}
                Err(_) => break,
                _ => panic!("bad Range unexpectedly completed"),
            }
        }
        let count = server.range_requests();
        assert!(
            source.resume_after_read_interruption().is_err(),
            "completed HTTP error was recovered as an interruption"
        );
        assert!(source.read(&tx, &state).await.is_err());
        assert_eq!(server.range_requests(), count);
    }
}
pub(super) async fn done(
    rx: &mut mpsc::Receiver<StreamMsg>,
) -> (Vec<f32>, crate::streaming::StreamRevision) {
    let mut pcm = Vec::new();
    loop {
        match tokio::time::timeout(WAIT, rx.recv())
            .await
            .unwrap()
            .expect("retained HTTP source closed before Done")
        {
            StreamMsg::Samples { pcm: chunk, .. } => pcm.extend_from_slice(&chunk),
            StreamMsg::Done { revision, .. } => return (pcm, revision),
            StreamMsg::Ready { .. } => {}
            StreamMsg::Error { message, .. } => panic!("retained Range revision error: {message}"),
        }
    }
}
pub(super) fn start_controlled(
    server: &Server,
    host: &Arc<migo_io::scheduler::IoScheduler>,
) -> (mpsc::Receiver<StreamMsg>, Arc<StreamingState>) {
    // A real session capability selects the same production URL prepare path.
    let root = std::env::temp_dir().join(format!("migo-range-direct-{}", std::process::id()));
    let paths =
        shared::vfs::GamePaths::new(root.join("files"), root.join("cache"), "audio", 1).unwrap();
    paths.ensure_directories().unwrap();
    let spool = migo_services::audio::LocalAudioFile::session(host.clone(), &paths).http_spool();
    let state = StreamingState::new_url();
    let rx = crate::streaming::start_streaming_download_with_spool(
        host.audio_cpu(),
        server.client(host.pools().clone()),
        "http://indexed-product-fixture.invalid/direct".into(),
        state.clone(),
        16000,
        Some(spool),
    );
    (rx, state)
}
#[tokio::test]
async fn real_range_controls_reject_without_change_and_wait_for_actual_consumer_ack() {
    let data = include_bytes!("decoder/aac-fixtures/1-seek-front.m4a");
    let server = server(data.to_vec(), Mode::Normal, false, false).await;
    let host = Arc::new(migo_io::scheduler::IoScheduler::new(1089));
    let (mut rx, state) = start_controlled(&server, &host);
    let (first, rev) = done(&mut rx).await;
    assert_eq!(
        first,
        crate::decoder::decode_for_rate(data, 16000)
            .unwrap()
            .samples
    );
    let port = state
        .control_sender()
        .expect("pinned HTTP source never installed controls");
    let count = server.range_requests();
    for target in [-1, i64::MAX] {
        let reply = crate::stream_control::request(port, target, Instant::now() + WAIT)
            .unwrap()
            .await
            .unwrap();
        assert!(
            matches!(
                reply,
                crate::stream_control::Reply::Rejected(ErrorCode::InvalidArgument)
            ),
            "invalid HTTP seek changed the source"
        );
        assert_eq!(server.range_requests(), count);
        assert!(rx.try_recv().is_err());
    }
    let reply = crate::stream_control::request(port, 0, Instant::now() + WAIT)
        .unwrap()
        .await
        .unwrap();
    let crate::stream_control::Reply::Prepared(prepared) = reply else {
        panic!("HTTP restart was not prepared")
    };
    // Producer and barrier share the single IO worker. The handshake has polled
    // its receipt before this barrier finishes, with no timing-based absence test.
    crate::streaming::get_stream_runtime()
        .spawn(async {})
        .await
        .unwrap();
    assert!(
        !prepared.accept.is_closed(),
        "pinned HTTP dropped actual consumer ACK"
    );
    assert!(
        rx.try_recv().is_err(),
        "pinned HTTP published a revision before consumer ACK"
    );
    assert_ne!(prepared.revision, rev);
    prepared.accept.send(()).unwrap();
    let (pcm, new) = done(&mut rx).await;
    assert_eq!(pcm, first);
    assert_eq!(new, prepared.revision);
    let reply = crate::stream_control::request(port, 0, Instant::now() + WAIT)
        .unwrap()
        .await
        .unwrap();
    let crate::stream_control::Reply::Prepared(prepared) = reply else {
        panic!()
    };
    drop(prepared);
    assert!(
        tokio::time::timeout(WAIT, rx.recv())
            .await
            .unwrap()
            .is_none(),
        "dropped HTTP consumer ACK did not retire its source"
    );
    assert!(state.is_cancelled());
}
#[tokio::test]
async fn public_range_seek_keeps_original_deadline_through_the_first_real_body_chunk() {
    let server = server(padded(TAIL, 1024 * 1024), Mode::Normal, false, false).await;
    let host = Arc::new(migo_io::scheduler::IoScheduler::new(1090));
    let (mut rx, state) = start_controlled(&server, &host);
    done(&mut rx).await;
    let port = state.control_sender().unwrap();
    server.pause_range_body(1);
    let deadline = Instant::now() + Duration::from_millis(500);
    let reply = crate::stream_control::request(port, 0, deadline).unwrap();
    tokio::time::timeout(WAIT, server.entered.notified())
        .await
        .expect("public HTTP seek never reached the real Range body");
    tokio::select! {
        result=reply=>assert!(result.is_err(),"HTTP control prepared beyond its original body deadline"),
        _=tokio::time::sleep_until(deadline+Duration::from_millis(300))=>panic!("HTTP control renewed its original media deadline"),
    }
    assert!(
        matches!(
            tokio::time::timeout(WAIT, rx.recv()).await.unwrap(),
            Some(StreamMsg::Error { .. })
        ),
        "expired HTTP control did not publish its original revision error"
    );
    assert!(rx.recv().await.is_none());
}
