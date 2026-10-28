//! Every sequential codec consumes the same sealed HTTP source and player.
use crate::{
    local_source::FileReader,
    local_source_contracts::{self as local, Format},
    spool_contracts::{self as http, Reply},
    spool_source,
    streaming::{StreamingState, stream_channel},
    streaming_player_contracts::{self as playback, PlaybackCase, StreamInput},
};
use migo_io::scheduler::IoScheduler;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

async fn open(
    name: &str,
    owner: &IoScheduler,
    chunked: bool,
    tag: bool,
    fragment: usize,
) -> FileReader {
    let reply = Reply::framing(chunked, tag);
    let server = http::server(std::fs::read(local::assets().join(name)).unwrap(), reply).await;
    let (tx, mut rx) = stream_channel();
    let state = StreamingState::new();
    let mut reader = spool_source::http(
        http::client(),
        server.url.clone(),
        owner,
        http::limits(name, 4 * 1024 * 1024),
        &tx,
        &state,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        rx.try_recv().is_err(),
        "spool published before decoder admission"
    );
    assert_eq!(server.requests.load(Ordering::Acquire), 1);
    reader.chunk_bytes = fragment;
    reader
}

#[tokio::test]
async fn all_formats_framings_validators_partitions_and_rates_share_the_sealed_source() {
    let mut count = 0;
    for (format, ext) in [
        (Format::Mp3, "mp3"),
        (Format::Wave, "wav"),
        (Format::Adts, "aac"),
        (Format::Vorbis, "ogg"),
    ] {
        for i in 0..4 {
            let name = format!("{i}.{ext}");
            let data = std::fs::read(local::assets().join(&name)).unwrap();
            let rate = crate::candidate::decode(&data, 16_777_216)
                .unwrap()
                .sample_rate;
            for target in [rate, if rate == 48000 { 44100 } else { 48000 }] {
                let (expected, _) = local::reference(&data, target);
                for chunked in [false, true] {
                    for tag in [false, true] {
                        for fragment in [137, 65536] {
                            let owner = IoScheduler::new(0);
                            let reader = open(&name, &owner, chunked, tag, fragment).await;
                            let got = local::receive(format, owner, reader, target).await;
                            assert_eq!(
                                (got.ready, got.done, got.errors.len()),
                                (1, 1, 0),
                                "{name}/{chunked}/{tag}/{fragment}/{target}: {:?}",
                                got.errors
                            );
                            assert_eq!(
                                got.pcm, expected,
                                "{name}/{chunked}/{tag}/{fragment}/{target}"
                            );
                            count += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(count, 256);
    println!("W09_SPOOL_FORMAT_PCM {count} exact comparisons");
}

#[tokio::test]
async fn all_formats_sealed_players_preserve_pause_pcm_and_exact_ended() {
    let mut count = 0;
    for (format, name) in [
        (Format::Mp3, "3.mp3"),
        (Format::Wave, "3.wav"),
        (Format::Adts, "3.aac"),
        (Format::Vorbis, "2.ogg"),
    ] {
        for chunked in [false, true] {
            for target in [44100, 48000] {
                let (expected, channels) =
                    local::reference(&std::fs::read(local::assets().join(name)).unwrap(), target);
                let frames = expected.len() / channels as usize;
                let owner = IoScheduler::new(0);
                let reader = open(name, &owner, chunked, false, 65536).await;
                let state = StreamingState::new();
                let (tx, rx) = stream_channel();
                let steps = Arc::new(AtomicUsize::new(0));
                let task = local::spawn(
                    format,
                    owner,
                    reader,
                    target,
                    tx,
                    state.clone(),
                    steps.clone(),
                );
                playback::consume_player::<false>(
                    PlaybackCase {
                        name,
                        source_name: format!("spool://{name}/{chunked}"),
                        target,
                        pause: true,
                        long: false,
                        expected,
                        channels,
                        frames,
                    },
                    StreamInput {
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
    println!("W09_SPOOL_FORMAT_PLAYER {count} cases");
}

#[tokio::test]
async fn malformed_sealed_formats_publish_one_error_without_done() {
    for (format, name) in [
        (Format::Mp3, "bad.mp3"),
        (Format::Wave, "bad.wav"),
        (Format::Adts, "bad.aac"),
        (Format::Vorbis, "bad.ogg"),
    ] {
        for chunked in [false, true] {
            let owner = IoScheduler::new(0);
            let reader = open(name, &owner, chunked, false, 137).await;
            let got = local::receive(format, owner, reader, 48000).await;
            assert_eq!((got.done, got.errors.len()), (0, 1), "{name}/{chunked}");
        }
    }
}
