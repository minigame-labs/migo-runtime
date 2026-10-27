//! Test the cache against real media and an independent fixture-layout oracle.
//! The HTTP collector remains test-only; resident encoded memory is measured
//! separately and never confused with this oracle's accumulated PCM.
use super::*;
use crate::push_decoder::BufferedM4aDecoder;
use std::sync::atomic::AtomicBool;

fn sendable<T: Send>(future: T) -> T {
    future
}

async fn buffered(server: &Server, fragment: usize) -> Result<Decoded, String> {
    let (tx, _rx) = stream_channel();
    tokio::time::timeout(
        Duration::from_secs(10),
        sendable(decode_mode::<true>(
            client(),
            server.url.clone(),
            fragment,
            Limits::default(),
            tx,
            StreamingState::new(),
            Arc::new(Notify::new()),
            Duration::from_secs(2),
        )),
    )
    .await
    .expect("buffered decoder must make bounded progress")
    .map(|r| r.expect("unexpected cancellation"))
}
fn collect(data: &[u8], cap: Option<u64>) -> Result<(Vec<f32>, (usize, usize, u64, u64)), String> {
    let mut decoder = BufferedM4aDecoder::with_object_len(cap, Some(data.len() as u64));
    assert!(data.len() <= 65536);
    assert_eq!(decoder.push(data)?, data.len());
    decoder.finish_input();
    let mut out = Vec::with_capacity(SAMPLES_PER_STEP);
    let mut pcm = Vec::new();
    for _ in 0..4096 {
        out.clear();
        let result = decoder.drain(&mut out, &AtomicBool::new(false))?;
        pcm.extend_from_slice(&out);
        assert!(
            decoder.buffered_metrics().3 <= 8,
            "buffered scan restarted instead of advancing"
        );
        match result {
            Progress::Yield => {}
            Progress::Finished => return Ok((pcm, decoder.buffered_metrics())),
            other => panic!("complete bounded asset requested IO: {other:?}"),
        }
    }
    panic!("buffered decoder never completed")
}

#[tokio::test]
async fn small_assets_reuse_one_response_without_validators_or_range() {
    let mut comparisons = 0;
    for name in [
        "0-front.m4a",
        "0-tail.m4a",
        "1-tail.m4a",
        "1-reorder.m4a",
        "2-tail.m4a",
        "2-reverse.m4a",
        "2-multi.m4a",
        "2-tail-reverse.m4a",
        "2-tail-multi.m4a",
        "2-buffered-many.m4a",
    ] {
        let data = fixture(name);
        assert!(data.len() <= 65536, "bounded fixture {name}");
        let expected = candidate::decode(&data, 16_777_216).unwrap().samples;
        for chunked in [false, true] {
            for fragment in [137, 65536] {
                let server = start_server(
                    data.clone(),
                    Config {
                        no_tag: true,
                        chunked,
                        fault: Fault::IgnoredRange,
                        ..Config::default()
                    },
                )
                .await;
                let decoded = buffered(&server, fragment).await.unwrap();
                assert_eq!(decoded.pcm, expected, "{name}/{chunked}/{fragment}");
                assert!(
                    server.ranges.lock().unwrap().is_empty(),
                    "buffer reuse issued HTTP Range"
                );
                assert_eq!(
                    (decoded.indexes, decoded.resumes, decoded.media_requests),
                    (0, 0, 0)
                );
                assert!(decoded.buffer.0 <= 65536 && decoded.buffer.1 <= 65536);
                if name != "0-front.m4a" {
                    assert!(decoded.buffer.2 > 0, "no buffered source resume");
                }
                comparisons += 1;
            }
        }
    }
    assert_eq!(comparisons, 40);
    println!("W09_M4A_BUFFERED_HTTP {comparisons} exact PCM comparisons without Range/ETag");
}

#[tokio::test]
async fn compaction_keeps_absolute_positions_and_avoids_redundant_io() {
    let data = fixture("1-buffered-prefix.m4a");
    assert!(data.len() > 128 * 1024);
    let expected = candidate::decode(&data, 16_777_216).unwrap().samples;
    for chunked in [false, true] {
        for fragment in [137, 8192] {
            let server = start_server(
                data.clone(),
                Config {
                    no_tag: true,
                    chunked,
                    ..Config::default()
                },
            )
            .await;
            let decoded = buffered(&server, fragment).await.unwrap();
            assert_eq!(decoded.pcm, expected);
            assert!(decoded.buffer.2 > 0);
            assert!(decoded.buffer.0 <= 65536 && decoded.buffer.1 <= 65536);
            assert!(server.ranges.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn genuine_misses_keep_pinned_http_and_large_indexes_stop_prefetch_at_capacity() {
    let data = fixture("2-buffered-miss.m4a");
    let expected = candidate::decode(&data, 16_777_216).unwrap().samples;
    for chunked in [false, true] {
        let server = start_server(
            data.clone(),
            Config {
                chunked,
                ..Config::default()
            },
        )
        .await;
        let decoded = buffered(&server, 137).await.unwrap();
        assert_eq!(decoded.pcm, expected);
        assert!(decoded.resumes > 0 && decoded.media_requests > 0);
        assert!(decoded.buffer.0 <= 65536 && decoded.buffer.1 <= 65536);
    }
    let server = start_server(
        data,
        Config {
            no_tag: true,
            ..Config::default()
        },
    )
    .await;
    assert!(
        buffered(&server, 137)
            .await
            .err()
            .is_some_and(|e| e.contains("NoStrongValidator"))
    );
    let data = fixture("1-metadata.m4a");
    let expected = candidate::decode(&data, 16_777_216).unwrap().samples;
    let movie = atom(&data, AtomKind::Movie);
    let server = start_server(data, Config::default()).await;
    let decoded = buffered(&server, 137).await.unwrap();
    assert_eq!(decoded.pcm, expected);
    assert_eq!(decoded.indexes, 1);
    assert!(
        decoded.first_source_peak.unwrap() < 65536,
        "oversized moov filled the entire prefetch cache"
    );
    assert!(decoded.first_source_peak.unwrap() as u64 <= movie.offset + 16 + 137);
}

#[test]
fn buffered_index_scanning_yields_and_cancellation_stays_sticky() {
    // A no-op push before EOF must not compact away the original prefix.
    // Cross-mdat discovery later still needs that prefix in the same window.
    let full = fixture("2-buffered-full.m4a");
    assert_eq!(full.len(), 65536);
    let expected = candidate::decode(&full, 16_777_216).unwrap().samples;
    let mut decoder = BufferedM4aDecoder::with_object_len(None, Some(full.len() as u64));
    decoder.push(&full).unwrap();
    let mut out = Vec::with_capacity(SAMPLES_PER_STEP);
    assert_eq!(
        decoder.drain(&mut out, &AtomicBool::new(false)).unwrap(),
        Progress::Yield
    );
    assert!(out.is_empty());
    assert_eq!(decoder.push(&[]).unwrap(), 0);
    decoder.finish_input();
    let mut pcm = Vec::new();
    loop {
        out.clear();
        let progress = decoder.drain(&mut out, &AtomicBool::new(false)).unwrap();
        pcm.extend_from_slice(&out);
        match progress {
            Progress::Yield => {}
            Progress::Finished => break,
            other => panic!("empty push discarded buffered prefix: {other:?}"),
        }
    }
    assert_eq!(pcm, expected);
    for extended in [false, true] {
        let data = with_padding(fixture("1-tail.m4a"), 100, 0, extended);
        let expected = candidate::decode(&data, 16_777_216).unwrap().samples;
        let (actual, metrics) = collect(&data, None).unwrap();
        assert_eq!(actual, expected);
        assert!(
            metrics.3 >= 3 && metrics.3 <= 8,
            "header quantum was skipped or probe restarted"
        );
        let mut decoder = BufferedM4aDecoder::with_object_len(None, Some(data.len() as u64));
        decoder.push(&data).unwrap();
        decoder.finish_input();
        let mut out = Vec::with_capacity(SAMPLES_PER_STEP);
        assert_eq!(
            decoder.drain(&mut out, &AtomicBool::new(false)).unwrap(),
            Progress::Yield
        );
        assert!(out.is_empty());
        assert_eq!(
            decoder.drain(&mut out, &AtomicBool::new(true)).unwrap(),
            Progress::Cancelled
        );
        assert_eq!(
            decoder.drain(&mut out, &AtomicBool::new(false)).unwrap(),
            Progress::Cancelled
        );
        assert!(decoder.push(b"late").is_err());
    }
}

#[test]
fn buffered_eof_quota_and_failed_state_do_not_hide_errors() {
    let data = fixture("1-tail.m4a");
    let expected = candidate::decode(&data, 16_777_216).unwrap().samples;
    assert_eq!(
        collect(&data, Some(expected.len() as u64)).unwrap().0,
        expected
    );
    assert!(collect(&data, Some(expected.len() as u64 - 1)).is_err());
    for known in [false, true] {
        for clipped in [1, 3, 8, 16] {
            let mut decoder =
                BufferedM4aDecoder::with_object_len(None, known.then_some(data.len() as u64));
            decoder.push(&data[..data.len() - clipped]).unwrap();
            decoder.finish_input();
            let mut out = Vec::with_capacity(SAMPLES_PER_STEP);
            let mut failed = false;
            for _ in 0..1024 {
                out.clear();
                match decoder.drain(&mut out, &AtomicBool::new(false)) {
                    Err(_) => {
                        failed = true;
                        break;
                    }
                    Ok(Progress::Yield) => {}
                    other => panic!("truncated buffered movie did not fail: {other:?}"),
                }
            }
            assert!(failed && out.is_empty());
            assert!(decoder.drain(&mut out, &AtomicBool::new(false)).is_err());
            assert!(decoder.push(b"late").is_err());
        }
    }
}

#[test]
fn steady_buffered_rewinds_use_no_extra_rust_heap_allocation() {
    let data = fixture("2-buffered-many.m4a");
    let mut decoder = BufferedM4aDecoder::with_object_len(None, Some(data.len() as u64));
    decoder.push(&data).unwrap();
    decoder.finish_input();
    let mut out = Vec::with_capacity(SAMPLES_PER_STEP);
    let cancel = AtomicBool::new(false);
    for _ in 0..128 {
        out.clear();
        assert_eq!(decoder.drain(&mut out, &cancel).unwrap(), Progress::Yield);
        if decoder.buffered_metrics().2 >= 64 {
            break;
        }
    }
    assert!(
        decoder.buffered_metrics().2 >= 64,
        "codec did not warm through real buffered packet resumes"
    );
    let seen = decoder.buffered_metrics().2;
    let before = migo_alloc_probe::thread_counts().allocation_events();
    for _ in 0..16 {
        out.clear();
        assert_eq!(decoder.drain(&mut out, &cancel).unwrap(), Progress::Yield);
        assert!(!out.is_empty());
    }
    let allocations = migo_alloc_probe::thread_counts().allocation_events() - before;
    assert_eq!(
        allocations, 0,
        "buffered positioning allocated after codec warmup"
    );
    let metrics = decoder.buffered_metrics();
    assert!(metrics.2 >= seen + 96);
    assert!(metrics.0 <= 65536 && metrics.1 <= 65536);
    println!(
        "W09_M4A_BUFFERED_STEADY {} source resumes,0 Rust allocation events",
        metrics.2 - seen
    );
}

#[tokio::test]
async fn controlled_delay_abba_preserves_pcm_and_reduces_range_roundtrips() {
    for name in ["1-tail.m4a", "2-tail-multi.m4a"] {
        let data = fixture(name);
        let expected = candidate::decode(&data, 16_777_216).unwrap().samples;
        for mode in [false, true, true, false, true, false, false, true] {
            let server = start_server(
                data.clone(),
                Config {
                    fault: Fault::Slow,
                    ..Config::default()
                },
            )
            .await;
            let (tx, _rx) = stream_channel();
            let start = Instant::now();
            let decoded = if mode {
                decode_mode::<true>(
                    client(),
                    server.url.clone(),
                    8192,
                    Limits::default(),
                    tx,
                    StreamingState::new(),
                    Arc::new(Notify::new()),
                    Duration::from_secs(2),
                )
                .await
            } else {
                decode_mode::<false>(
                    client(),
                    server.url.clone(),
                    8192,
                    Limits::default(),
                    tx,
                    StreamingState::new(),
                    Arc::new(Notify::new()),
                    Duration::from_secs(2),
                )
                .await
            };
            let elapsed = start.elapsed();
            let decoded = decoded.unwrap().unwrap();
            assert_eq!(decoded.pcm, expected);
            let requests = server.ranges.lock().unwrap().len();
            if mode {
                assert_eq!(requests, 0);
            } else {
                assert!(requests >= 2);
            }
            println!(
                "W09_M4A_BUFFERED_ABBA {{\"asset\":\"{name}\",\"buffered\":{mode},\"range_requests\":{requests},\"wall_ns\":{}}}",
                elapsed.as_nanos()
            );
        }
    }
}

#[test]
fn library_buffer_windows_reject_overflow_and_latch_failure() {
    use symphonia_format_isomp4::streaming::{PacketStep, Packetizer, PacketizerOptions};
    let data = fixture("1-tail.m4a");
    for overflow in [false, true] {
        let mut parser = Packetizer::new(PacketizerOptions::default());
        let mut at = 0;
        loop {
            match parser.next(&data[at..], false).unwrap() {
                PacketStep::Discard(n) => at += n,
                PacketStep::NeedIndex(_) => break,
                _ => panic!("expected a pending tail index"),
            }
        }
        let result = if overflow {
            parser.try_buffered(u64::MAX - 4, &[0; 8], 16)
        } else {
            parser.try_buffered(0, &data, data.len() - 1)
        };
        assert!(result.is_err());
        assert!(parser.next(&data[at..], true).is_err());
        assert!(parser.try_buffered(0, &data, data.len()).is_err());
    }
    let mut parser = Packetizer::new(PacketizerOptions::default());
    let mut at = 0;
    loop {
        match parser.next(&data[at..], false).unwrap() {
            PacketStep::Discard(n) => at += n,
            PacketStep::NeedIndex(_) => break,
            _ => panic!("expected a pending index"),
        }
    }
    assert!(matches!(
        parser.try_buffered(u64::MAX - 16, &[0; 8], 65536).unwrap(),
        symphonia_format_isomp4::streaming::BufferedStep::Unavailable
    ));
    assert!(matches!(
        parser.try_buffered(0, &data, data.len()).unwrap(),
        symphonia_format_isomp4::streaming::BufferedStep::Index
    ));
}
