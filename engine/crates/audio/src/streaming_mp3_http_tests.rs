//! Real original HTTP response, with no format-specific network adapter.
use super::*;
const MUSIC: &[u8] = include_bytes!("decoder/mp3/fixtures/music.mp3");

#[tokio::test]
async fn progressive_gapless_mp3_uses_the_original_body_and_complete_product_presentation() {
    let host = migo_io::scheduler::IoScheduler::new(1065);
    for (name, data, modes) in [
        (
            "music",
            MUSIC,
            vec![Mode::Normal, Mode::NoTag, Mode::Chunked],
        ),
        (
            "mpeg2",
            include_bytes!("decoder/mp3/fixtures/gapless-mpeg2.mp3").as_slice(),
            vec![Mode::Normal, Mode::Chunked, Mode::Gzip],
        ),
    ] {
        for (mode_id, mode) in modes.into_iter().enumerate() {
            for rate in [16000, 48000] {
                let server = server(data.to_vec(), mode, false, false).await;
                let (mut rx, state) = start(&server, &host, rate);
                let actual = collect(&mut rx)
                    .await
                    .expect("HTTP MP3 failed its complete presentation");
                let expected = crate::decoder::decode_for_rate(data, rate).unwrap();
                assert_eq!(
                    actual, expected.samples,
                    "HTTP MP3 lost complete gapless PCM"
                );
                assert!(
                    state.timeline().is_some(),
                    "HTTP MP3 lost its authoritative clock"
                );
                assert!(state.control_sender().is_none() && !state.retains_source());
                assert_eq!(
                    server.range_requests(),
                    0,
                    "sequential MP3 opened an unrelated Range request"
                );
                if let Some(dir) = std::env::var_os("MIGO_MP3_GAPLESS_PCM_DIR") {
                    std::fs::write(
                        std::path::Path::new(&dir)
                            .join(format!("http-{name}-{mode_id}-{rate}.f32le")),
                        actual
                            .iter()
                            .flat_map(|x| x.to_le_bytes())
                            .collect::<Vec<_>>(),
                    )
                    .unwrap();
                }
            }
        }
    }
}

#[tokio::test]
async fn progressive_mp3_publishes_authoritative_pcm_while_original_response_is_held() {
    let host = migo_io::scheduler::IoScheduler::new(1066);
    let server = server_with_initial_prefix(MUSIC.to_vec(), Mode::NoTag, Some(32768), false).await;
    let (mut rx, state) = start(&server, &host, 44100);
    let expected = crate::decoder::decode_for_rate(MUSIC, 44100).unwrap();
    let mut pcm = Vec::new();
    let mut ready = false;
    while pcm.len() < 8192 {
        match tokio::time::timeout(WAIT, rx.recv())
            .await
            .unwrap()
            .unwrap()
        {
            StreamMsg::Ready { .. } => {
                ready = true;
                assert_eq!(
                    state.timeline(),
                    Some(crate::streaming::StreamTimeline::new(480137, 48000, 44100, 2).unwrap()),
                    "held MP3 Ready lost authoritative gapless clock"
                );
            }
            StreamMsg::Samples { pcm: data, .. } => {
                assert!(ready);
                pcm.extend_from_slice(&data);
            }
            StreamMsg::Done { .. } => panic!("held original MP3 response ended before EOF"),
            StreamMsg::Error { message, .. } => {
                panic!("held original MP3 response failed: {message}")
            }
        }
    }
    assert_eq!(server.initial_eof.load(Ordering::Acquire), 0);
    assert_eq!(
        pcm,
        &expected.samples[..pcm.len()],
        "held HTTP MP3 prefix differs from full frozen presentation"
    );
    assert_eq!(server.range_requests(), 0);
    state.cancel();
    drop(rx);
}
