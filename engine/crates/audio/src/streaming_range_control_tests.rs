//! Public pinned-Range controls use the original reader, CPU and PCM consumer.
use super::controls::{Context, player};
use super::*;
use crate::stream_open::file_tests::controls::{
    deterministic, finish, pull_seek, seek_reference, suffix, wait,
};
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
async fn pns_http_seeks_match_an_independent_pull_seek() {
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
/// A controlled source whose network stops after its first `prefix` bytes:
/// the initial body stalls, and so does every Range response. Returns once
/// the producer is waiting on bytes that will not come.
async fn stalled_after(
    data: &[u8],
    prefix: usize,
    scheduler: i32,
) -> (Server, mpsc::Receiver<StreamMsg>, Arc<StreamingState>) {
    let server = server_with_initial_prefix(data.to_vec(), Mode::Normal, Some(prefix), false).await;
    server.pause_range_body(1);
    let host = Arc::new(migo_io::scheduler::IoScheduler::new(scheduler));
    let (mut rx, state) = start_controlled(&server, &host);
    // Keep the channel drained so the producer reads on until the stall, which
    // shows as its range request for the missing bytes being held.
    let entered = server.entered.clone();
    let deadline = tokio::time::sleep(WAIT);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = entered.notified() => break,
            message = rx.recv() => match message {
                Some(StreamMsg::Error { message, .. }) => panic!("stalled source failed: {message}"),
                Some(_) => {}
                None => panic!("producer ended before the stall"),
            },
            _ = &mut deadline => panic!("the producer never waited on the stalled network"),
        }
    }
    (server, rx, state)
}

#[tokio::test]
async fn a_control_preempts_a_read_the_network_does_not_answer() {
    // WAVE through Symphonia's reader; MP3 through the frame source, whose
    // input window is what waits. A restart needs no seek beyond the start.
    let wave = crate::test_media::wav(16000, 1, 16000 * 8);
    let mp3 = include_bytes!("decoder/mp3/fixtures/music.mp3");
    for (data, prefix, scheduler) in [(&wave[..], 64 * 1024, 1094), (&mp3[..], 48 * 1024, 1096)] {
        preempted(data, prefix, scheduler).await;
    }
}

async fn preempted(data: &[u8], prefix: usize, scheduler: i32) {
    let (_server, mut rx, state) = stalled_after(data, prefix, scheduler).await;
    let asked = Instant::now();
    let reply =
        crate::stream_control::request(state.control_sender().unwrap(), 0, Instant::now() + WAIT)
            .unwrap();
    let crate::stream_control::Reply::Prepared(prepared) =
        tokio::time::timeout(Duration::from_secs(2), reply)
            .await
            .expect("a seek into delivered media waited behind a stalled read")
            .unwrap()
    else {
        panic!("a seek into delivered media was refused")
    };
    assert!(asked.elapsed() < Duration::from_secs(1));
    let revision = prepared.revision;
    prepared.accept.send(()).unwrap();
    // The new traversal plays the delivered bytes exactly, from the start.
    let reference = crate::decoder::decode_for_rate(data, 16000)
        .unwrap()
        .samples;
    let mut got = Vec::new();
    while got.len() < 16000 {
        match tokio::time::timeout(WAIT, rx.recv())
            .await
            .unwrap()
            .unwrap()
        {
            StreamMsg::Samples { revision: r, pcm } if r == revision => got.extend_from_slice(&pcm),
            StreamMsg::Error { message, .. } => panic!("preempted source failed: {message}"),
            _ => {}
        }
    }
    assert!(
        got[..16000] == reference[..16000],
        "the preempted reader resumed off its position"
    );
}

#[tokio::test]
async fn a_control_whose_media_never_arrives_fails_by_its_own_deadline() {
    // A sample-exact MP3 seek needs the frames before its target (the frame
    // index reaches it); a WAVE seek, by contrast, needs no media to prepare.
    // The stream is open and playing on the first 48 KiB when the network
    // stops, far short of 90% of the presentation.
    let data = include_bytes!("decoder/mp3/fixtures/music.mp3");
    let (_server, mut rx, state) = stalled_after(data, 48 * 1024, 1095).await;
    let target = state.timeline().unwrap().output_frames() as i64 * 9 / 10;
    let deadline = Instant::now() + Duration::from_millis(500);
    let reply =
        crate::stream_control::request(state.control_sender().unwrap(), target, deadline).unwrap();
    tokio::select! {
        result = reply => assert!(result.is_err(), "a seek was acknowledged without its media"),
        _ = tokio::time::sleep_until(deadline + Duration::from_millis(300)) => {
            panic!("a control outlived its deadline")
        }
    }
    loop {
        match tokio::time::timeout(WAIT, rx.recv()).await.unwrap() {
            Some(StreamMsg::Error { .. }) => break,
            Some(_) => {}
            None => panic!("the stream ended without its timeout error"),
        }
    }
    assert!(
        tokio::time::timeout(WAIT, rx.recv())
            .await
            .unwrap()
            .is_none(),
        "the producer outlived its failure"
    );
    assert!(!state.is_cancelled());
}
