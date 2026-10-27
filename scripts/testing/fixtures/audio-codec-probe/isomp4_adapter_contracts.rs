//! The common bounded decoder must suspend for IO without losing prefetched
//! bytes, AAC history, cancellation, or the exact container PCM timeline.
use crate::streaming::{M4aDecoder, M4aSourceRequest, Progress, SAMPLES_PER_STEP};
use std::sync::atomic::{AtomicBool, Ordering};
use symphonia_format_isomp4::streaming::{AtomKind, AtomSpan, AudioIndex, IndexOptions};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::PathBuf::from(std::env::var_os("W09_M4A_FIXTURES").unwrap()).join(name),
    )
    .unwrap()
}
fn span(data: &[u8], kind: AtomKind, byte: Option<u64>) -> AtomSpan {
    let mut at = 0;
    while at < data.len() {
        let atom = AtomSpan::read(&data[at..], at as u64, Some(data.len() as u64))
            .unwrap()
            .unwrap();
        if atom.kind == kind
            && byte.is_none_or(|byte| byte >= atom.body_offset && byte < atom.end.unwrap())
        {
            return atom;
        }
        at = atom.end.unwrap() as usize;
    }
    panic!("missing fixture atom");
}
fn index(data: &[u8], object_len: Option<u64>) -> AudioIndex {
    let movie = span(data, AtomKind::Movie, None);
    AudioIndex::from_moov(
        &data[movie.offset as usize..movie.end.unwrap() as usize],
        IndexOptions {
            object_len,
            ..Default::default()
        },
    )
    .unwrap()
}

#[derive(Default)]
struct Observed {
    index: usize,
    range: usize,
    first_pcm_at: Option<usize>,
}
fn decode(
    data: &[u8],
    fragment: usize,
    cap: Option<u64>,
    object_len: Option<u64>,
) -> Result<(Vec<f32>, Observed), String> {
    let mut decoder = M4aDecoder::with_object_len(cap, object_len);
    let cancel = AtomicBool::new(false);
    let mut out = Vec::with_capacity(SAMPLES_PER_STEP);
    let mut pcm = Vec::new();
    let mut observed = Observed::default();
    let mut position = 0;
    let mut eof = false;
    for _ in 0..data.len() * 4 + 1024 {
        out.clear();
        let progress = decoder.drain(&mut out, &cancel)?;
        assert!(out.len() <= SAMPLES_PER_STEP);
        if !out.is_empty() {
            observed.first_pcm_at.get_or_insert(position);
        }
        pcm.extend_from_slice(&out);
        match progress {
            Progress::NeedInput => {
                assert!(!eof, "EOF must not request more bytes");
                if position == data.len() {
                    decoder.finish_input();
                    eof = true;
                } else {
                    let end = (position + fragment).min(data.len());
                    let n = decoder.push(&data[position..end])?;
                    assert!(n > 0 && n <= 65536);
                    position += n;
                }
            }
            Progress::Yield => {}
            Progress::NeedSource => {
                let request = decoder.source_request().expect("explicit source request");
                assert!(decoder.push(b"wrong-position").is_err());
                out.clear();
                assert_eq!(decoder.drain(&mut out, &cancel)?, Progress::NeedSource);
                assert_eq!(decoder.source_request(), Some(request));
                assert!(out.is_empty());
                match request {
                    M4aSourceRequest::Index(_) => {
                        decoder.install_index(index(data, object_len))?;
                        observed.index += 1;
                    }
                    M4aSourceRequest::ReadAt(packet) => {
                        let media = span(data, AtomKind::MediaData, Some(packet.offset));
                        assert!(packet.offset + u64::from(packet.size) <= media.end.unwrap());
                        decoder.resume_at(media, packet.offset)?;
                        position = packet.offset as usize;
                        eof = false;
                        observed.range += 1;
                    }
                }
                assert!(decoder.source_request().is_none());
            }
            Progress::Finished => {
                assert!(eof && position == data.len());
                return Ok((pcm, observed));
            }
            Progress::Cancelled => panic!("unexpected cancellation"),
        }
    }
    panic!("common M4A decoder made no bounded progress");
}

#[test]
fn common_m4a_partitions_and_source_resumes_preserve_whole_pcm() {
    let mut comparisons = 0;
    for (n, frames, channels) in [(0, 17, 1), (1, 9737, 2), (2, 240137, 1)] {
        let modes = if n == 0 {
            &["front", "tail"][..]
        } else {
            &["front", "tail", "split", "reorder", "co64"][..]
        };
        for mode in modes {
            let data = fixture(&format!("{n}-{mode}.m4a"));
            let expected = crate::candidate::decode(&data, 16_777_216).unwrap().samples;
            assert_eq!(expected.len(), frames * channels);
            for object_len in [Some(data.len() as u64), None] {
                for fragment in [1, 3, 137, 8192, 100000] {
                    let (actual, seen) = decode(&data, fragment, None, object_len).unwrap();
                    assert_eq!(actual, expected, "{n}/{mode}/{fragment}/{object_len:?}");
                    assert_eq!(seen.index, usize::from(*mode == "tail"));
                    assert_eq!(seen.range, usize::from(*mode == "reorder"));
                    if n > 0 && *mode == "front" && fragment == 1 {
                        assert!(seen.first_pcm_at.unwrap() < data.len());
                    }
                    comparisons += 1;
                }
            }
        }
    }
    assert_eq!(comparisons, 120);
    println!("W09_M4A_COMMON {comparisons} exact whole/partition/source comparisons");
}

#[test]
fn common_m4a_reposition_discards_prefetched_tail_without_resetting_codec() {
    // A reordered mdat at object EOF leaves no pending bytes at ReadAt, so
    // forgetting to reset the input can escape a partition-only comparison.
    // Retain a real optional atom after it and prefetch that atom as well.
    let mut data = fixture("1-reorder.m4a");
    data.extend_from_slice(&2056_u32.to_be_bytes());
    data.extend_from_slice(b"free");
    data.extend_from_slice(&[0x5a; 2048]);
    let expected = crate::candidate::decode(&data, 16_777_216).unwrap().samples;
    for object_len in [Some(data.len() as u64), None] {
        let (actual, seen) = decode(&data, 100000, None, object_len).unwrap();
        assert_eq!(seen.range, 1);
        assert_eq!(actual, expected);
    }
}

fn waiting(data: &[u8]) -> M4aDecoder {
    assert!(data.len() < 65536);
    let mut decoder = M4aDecoder::with_object_len(None, Some(data.len() as u64));
    assert_eq!(decoder.push(data).unwrap(), data.len());
    decoder.finish_input();
    let mut out = Vec::with_capacity(SAMPLES_PER_STEP);
    for _ in 0..1024 {
        out.clear();
        match decoder.drain(&mut out, &AtomicBool::new(false)).unwrap() {
            Progress::Yield => {}
            Progress::NeedSource => return decoder,
            other => panic!("expected source request, got {other:?}"),
        }
    }
    panic!("source request not reached");
}

#[test]
fn common_m4a_invalid_source_resumes_latch_failure() {
    let tail = fixture("1-tail.m4a");
    let mut decoder = waiting(&tail);
    assert!(matches!(
        decoder.source_request(),
        Some(M4aSourceRequest::Index(_))
    ));
    assert!(
        decoder
            .install_index(index(&tail, Some(tail.len() as u64 + 1)))
            .is_err()
    );
    assert!(decoder.source_request().is_none());
    let mut out = Vec::with_capacity(SAMPLES_PER_STEP);
    assert!(decoder.drain(&mut out, &AtomicBool::new(false)).is_err());
    assert!(
        decoder
            .install_index(index(&tail, Some(tail.len() as u64)))
            .is_err()
    );
    assert!(out.is_empty() && decoder.push(&tail).is_err());

    let data = fixture("1-reorder.m4a");
    let mut decoder = waiting(&data);
    let Some(M4aSourceRequest::ReadAt(packet)) = decoder.source_request() else {
        panic!()
    };
    let mut media = span(&data, AtomKind::MediaData, Some(packet.offset));
    media.kind = AtomKind::Movie;
    assert!(decoder.resume_at(media, packet.offset).is_err());
    assert!(decoder.source_request().is_none());
    assert!(decoder.drain(&mut out, &AtomicBool::new(false)).is_err());
    assert!(out.is_empty());

    let mut decoder = M4aDecoder::new(None);
    assert!(decoder.install_index(index(&tail, None)).is_err());
    assert!(decoder.push(&tail).is_err());
}

#[test]
fn common_m4a_cancellation_wins_over_pending_source_wait() {
    let data = fixture("1-tail.m4a");
    let mut decoder = waiting(&data);
    let cancel = AtomicBool::new(true);
    let mut out = Vec::with_capacity(SAMPLES_PER_STEP);
    assert_eq!(
        decoder.drain(&mut out, &cancel).unwrap(),
        Progress::Cancelled
    );
    cancel.store(false, Ordering::Release);
    assert_eq!(
        decoder.drain(&mut out, &cancel).unwrap(),
        Progress::Cancelled
    );
    assert!(decoder.source_request().is_none());
    assert!(
        decoder
            .install_index(index(&data, Some(data.len() as u64)))
            .is_err()
    );
    assert!(decoder.push(&data).is_err() && out.is_empty());
}

#[test]
fn common_m4a_large_metadata_and_exact_quota_remain_bounded() {
    let clean = fixture("1-front.m4a");
    let expected = crate::candidate::decode(&clean, 16_777_216)
        .unwrap()
        .samples;
    for name in ["1-metadata.m4a", "1-metadata-front.m4a"] {
        let data = fixture(name);
        assert!(data.len() > 1024 * 1024);
        let (actual, _) = decode(
            &data,
            137,
            Some(expected.len() as u64),
            Some(data.len() as u64),
        )
        .unwrap();
        assert_eq!(actual, expected);
        assert!(
            decode(
                &data,
                8192,
                Some(expected.len() as u64 - 1),
                Some(data.len() as u64)
            )
            .is_err()
        );
    }
    for n in 1..=16 {
        assert!(
            decode(
                &clean[..clean.len() - n],
                137,
                None,
                Some(clean.len() as u64)
            )
            .is_err()
        );
    }
}

#[test]
fn common_m4a_push_decode_convert_has_no_steady_rust_allocation() {
    let data = fixture("2-front.m4a");
    let mut packets = index(&data, Some(data.len() as u64));
    let mut decoder = M4aDecoder::with_object_len(None, Some(data.len() as u64));
    let mut out = Vec::with_capacity(SAMPLES_PER_STEP);
    let cancel = AtomicBool::new(false);
    let first = packets.next_packet().unwrap().unwrap();
    decoder.push(&data[..first.offset as usize]).unwrap();
    while decoder.drain(&mut out, &cancel).unwrap() == Progress::Yield {
        assert!(out.is_empty());
    }
    let mut current = Some(first);
    let mut step = || {
        let packet = current
            .take()
            .unwrap_or_else(|| packets.next_packet().unwrap().unwrap());
        let bytes =
            &data[packet.offset as usize..(packet.offset + u64::from(packet.size)) as usize];
        assert_eq!(decoder.push(bytes).unwrap(), bytes.len());
        out.clear();
        assert_eq!(
            decoder.drain(&mut out, &cancel).unwrap(),
            Progress::NeedInput
        );
        out.len()
    };
    for _ in 0..8 {
        step();
    }
    let before = migo_alloc_probe::thread_counts().allocation_events();
    for _ in 0..96 {
        assert!(step() > 0);
    }
    assert_eq!(
        migo_alloc_probe::thread_counts().allocation_events() - before,
        0
    );
}
