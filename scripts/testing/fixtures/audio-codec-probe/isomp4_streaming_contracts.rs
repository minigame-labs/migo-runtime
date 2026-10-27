#[path = "isomp4_index_contracts.rs"]
mod index;
use symphonia_codec_aac::AacDecoder;
use symphonia_core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia_format_isomp4::streaming::{
    AtomKind, AtomSpan, AudioIndex, IndexOptions, PacketStep, Packetizer, PacketizerOptions,
};

#[derive(Default)]
struct Observed {
    index_requests: usize,
    read_at: usize,
    first_pcm_at: Option<usize>,
    packets: usize,
}

fn media(data: &[u8], offset: u64, size: u32) -> Result<AtomSpan, String> {
    let mut at = 0;
    while at < data.len() {
        let span = AtomSpan::read(&data[at..], at as u64, Some(data.len() as u64))
            .map_err(|e| e.to_string())?
            .ok_or("short atom")?;
        if span.kind == AtomKind::MediaData
            && offset >= span.body_offset
            && offset + u64::from(size) <= span.end.unwrap()
        {
            return Ok(span);
        }
        at = span.end.unwrap() as usize;
    }
    Err("requested bytes are outside every actual mdat".into())
}

fn run(
    data: &[u8],
    fragment: usize,
    object_len: Option<u64>,
    provided: Option<AudioIndex>,
) -> Result<(Vec<f32>, Observed), String> {
    run_options(
        data,
        fragment,
        provided,
        PacketizerOptions {
            index: IndexOptions {
                object_len,
                ..Default::default()
            },
            ..Default::default()
        },
    )
}
fn run_options(
    data: &[u8],
    fragment: usize,
    provided: Option<AudioIndex>,
    options: PacketizerOptions,
) -> Result<(Vec<f32>, Observed), String> {
    let object_len = options.index.object_len;
    let mut parser = Packetizer::new(options);
    let mut supplied = provided;
    let mut decoder = None;
    let mut pcm = Vec::new();
    let mut seen = Observed::default();
    let (mut offset, mut available) = (0, 0);
    for _ in 0..10_000_000 {
        assert_eq!(parser.position(), offset as u64);
        let before = parser.position();
        match parser
            .next(&data[offset..available], available == data.len())
            .map_err(|e| e.to_string())?
        {
            PacketStep::NeedMore => {
                assert_eq!(parser.position(), before);
                if available == data.len() {
                    return Err("EOF requested more bytes".into());
                }
                available = (available + fragment).min(data.len()).min(offset + 65536);
                assert!(available > offset);
            }
            PacketStep::Discard(n) => {
                assert!(n > 0 && n <= 4096 && n <= available - offset);
                offset += n;
            }
            PacketStep::Packet { consumed, packet } => {
                assert!(consumed > 0 && consumed <= available - offset);
                assert_eq!(packet.data.as_ptr(), data[offset..].as_ptr());
                let codec = decoder.get_or_insert_with(|| {
                    AacDecoder::try_new(
                        parser
                            .track()
                            .unwrap()
                            .codec_params
                            .as_ref()
                            .unwrap()
                            .audio()
                            .unwrap(),
                        &AudioDecoderOptions::default().gapless(true),
                    )
                    .unwrap()
                });
                let block = codec.decode_ref(&packet).map_err(|e| e.to_string())?;
                let channels = block.spec().channels().count();
                let n = block.samples_interleaved();
                if n > 0 {
                    assert_eq!(
                        packet.pts.get() + packet.trim_start.get() as i64,
                        (pcm.len() / channels) as i64
                    );
                    assert_eq!(packet.dur.get(), (n / channels) as u64);
                    seen.first_pcm_at.get_or_insert(available);
                }
                let start = pcm.len();
                pcm.resize(start + n, 0.);
                block.copy_to_slice_interleaved(&mut pcm[start..]);
                offset += consumed;
                seen.packets += 1;
            }
            PacketStep::NeedIndex(request) => {
                assert_eq!(parser.position(), before);
                match parser
                    .next(&data[offset..available], available == data.len())
                    .unwrap()
                {
                    PacketStep::NeedIndex(again) => assert_eq!(again, request),
                    _ => panic!("index wait advanced"),
                }
                assert_eq!(parser.position(), before);
                let idx = match supplied.take() {
                    Some(idx) => idx,
                    None => AudioIndex::from_moov(
                        index::moov(data),
                        IndexOptions {
                            object_len,
                            ..Default::default()
                        },
                    )
                    .map_err(|e| e.to_string())?,
                };
                parser.install_index(idx).map_err(|e| e.to_string())?;
                seen.index_requests += 1;
            }
            PacketStep::ReadAt(packet) => {
                assert_eq!(parser.position(), before);
                match parser
                    .next(&data[offset..available], available == data.len())
                    .unwrap()
                {
                    PacketStep::ReadAt(again) => assert_eq!(again, packet),
                    _ => panic!("range wait advanced"),
                }
                let span = media(data, packet.offset, packet.size)?;
                parser
                    .resume_at(span, packet.offset)
                    .map_err(|e| e.to_string())?;
                offset = packet.offset as usize;
                available = offset;
                seen.read_at += 1;
            }
            PacketStep::End => {
                assert_eq!(offset, data.len());
                assert_eq!(parser.position(), data.len() as u64);
                return Ok((pcm, seen));
            }
        }
    }
    Err("no bounded forward progress".into())
}

#[test]
fn sequential_partitions_and_explicit_index_range_actions_preserve_exact_pcm() {
    let mut comparisons = 0;
    for (n, frames, channels) in [(0, 17_usize, 1), (1, 9737, 2), (2, 240137, 1)] {
        let modes = if n == 0 {
            &["front", "tail"][..]
        } else {
            &["front", "tail", "split", "reorder", "co64"][..]
        };
        for mode in modes {
            let data = index::file(&format!("{n}-{mode}.m4a"));
            let mut full = index::pull(&data);
            let mut codec = index::codec(&index::index(&data));
            use symphonia_core::formats::FormatReader;
            let mut expected = Vec::new();
            while let Some(packet) = full.next_packet().unwrap() {
                let block = codec.decode(&packet).unwrap();
                let start = expected.len();
                expected.resize(start + block.samples_interleaved(), 0.);
                block.copy_to_slice_interleaved(&mut expected[start..]);
            }
            assert_eq!(expected.len(), frames * channels);
            for fragment in [1, 3, 137, 8192, 65536] {
                let (pcm, seen) = run(&data, fragment, Some(data.len() as u64), None).unwrap();
                assert_eq!(pcm, expected, "{n}/{mode}/{fragment}");
                assert_eq!(seen.index_requests, usize::from(*mode == "tail"));
                assert_eq!(seen.read_at, usize::from(*mode == "reorder"));
                if n > 0 && *mode == "front" && fragment == 1 {
                    assert!(seen.first_pcm_at.unwrap() < data.len());
                }
                comparisons += 1;
            }
        }
    }
    assert_eq!(comparisons, 60);
}

#[test]
fn unknown_object_length_and_zero_sized_media_use_successful_object_eof() {
    let data = index::file("1-front.m4a");
    let (expected, _) = run(&data, 137, None, None).unwrap();
    let first = media(
        &data,
        index::index(&data).next_packet().unwrap().unwrap().offset,
        1,
    )
    .unwrap();
    let mut open = data.clone();
    open[first.offset as usize..first.offset as usize + 4].fill(0);
    assert_eq!(run(&open, 1, None, None).unwrap().0, expected);
    assert_eq!(
        run(&open, 137, Some(open.len() as u64), None).unwrap().0,
        expected
    );
}

#[test]
fn truncated_atoms_samples_and_terminal_tails_never_become_successful_end() {
    let data = index::file("1-front.m4a");
    for n in 1..=16 {
        assert!(run(&data[..data.len() - n], 137, Some(data.len() as u64), None).is_err());
    }
    for n in 1..8 {
        let mut trailing = data.clone();
        trailing.extend(vec![0; n]);
        assert!(run(&trailing, 137, Some(trailing.len() as u64), None).is_err());
    }
    // Entire decoded audio can arrive while the declared object still has an
    // unreceived trailing atom. A known length must not silently become Done.
    assert!(run(&data, 137, Some(data.len() as u64 + 24), None).is_err());
    let tail = index::file("1-tail.m4a");
    let provided = index::index(&tail);
    assert!(
        run(
            &tail[..tail.len() - 8],
            137,
            Some(tail.len() as u64),
            Some(provided)
        )
        .is_err()
    );
    let first = index::index(&data).next_packet().unwrap().unwrap();
    let span = media(&data, first.offset, first.size).unwrap();
    let mut crossing = data.clone();
    let size = (span.header_len as u32) + first.size - 1;
    crossing[span.offset as usize..span.offset as usize + 4].copy_from_slice(&size.to_be_bytes());
    assert!(run(&crossing, 137, Some(crossing.len() as u64), None).is_err());
    let mut parser = Packetizer::new(PacketizerOptions {
        index: IndexOptions {
            object_len: Some(crossing.len() as u64),
            ..Default::default()
        },
        ..Default::default()
    });
    let mut at = 0;
    loop {
        match parser.next(&crossing[at..], true) {
            Ok(PacketStep::Discard(n)) => at += n,
            Err(_) => break,
            _ => panic!("sample outside mdat was published before rejection"),
        }
    }
}

#[test]
fn sequential_packet_decode_and_convert_has_no_steady_rust_allocation() {
    let data = index::file("2-split.m4a");
    let mut parser = Packetizer::new(PacketizerOptions {
        index: IndexOptions {
            object_len: Some(data.len() as u64),
            ..Default::default()
        },
        ..Default::default()
    });
    let mut codec = None;
    let mut scratch = vec![0.; 2048];
    let mut offset = 0;
    let mut step = || loop {
        match parser.next(&data[offset..], true).unwrap() {
            PacketStep::Discard(n) => offset += n,
            PacketStep::Packet { consumed, packet } => {
                let dec = codec.get_or_insert_with(|| {
                    AacDecoder::try_new(
                        parser
                            .track()
                            .unwrap()
                            .codec_params
                            .as_ref()
                            .unwrap()
                            .audio()
                            .unwrap(),
                        &AudioDecoderOptions::default().gapless(true),
                    )
                    .unwrap()
                });
                let block = dec.decode_ref(&packet).unwrap();
                let n = block.samples_interleaved();
                block.copy_to_slice_interleaved(&mut scratch[..n]);
                offset += consumed;
                break;
            }
            _ => panic!("measurement fixture ended early"),
        }
    };
    for _ in 0..8 {
        step();
    }
    let before = migo_alloc_probe::thread_counts().allocation_events();
    for _ in 0..96 {
        step();
    }
    assert_eq!(
        migo_alloc_probe::thread_counts().allocation_events() - before,
        0
    );
}

#[test]
fn large_optional_metadata_is_bounded_and_cold_atom_storage_is_released() {
    let plain = index::file("1-front.m4a");
    let large = index::file("1-metadata-front.m4a");
    let tail = index::file("1-metadata.m4a");
    let expected = run(&plain, 137, Some(plain.len() as u64), None).unwrap().0;
    for data in [&large, &tail] {
        for n in [137, 65536] {
            assert_eq!(
                run(data, n, Some(data.len() as u64), None).unwrap().0,
                expected
            );
        }
    }
    let resident = |data: &[u8]| {
        let before = migo_alloc_probe::thread_counts().live_bytes();
        let mut p = Packetizer::new(PacketizerOptions {
            index: IndexOptions {
                object_len: Some(data.len() as u64),
                ..Default::default()
            },
            ..Default::default()
        });
        let mut offset = 0;
        loop {
            match p.next(&data[offset..], true).unwrap() {
                PacketStep::Discard(n) => offset += n,
                PacketStep::Packet { .. } => {
                    return migo_alloc_probe::thread_counts().live_bytes() - before;
                }
                _ => panic!("no packet"),
            }
        }
    };
    let small = resident(&plain);
    let big = resident(&large);
    assert!(
        big <= small + 65536,
        "cold moov buffer retained: {small}/{big}"
    );
    let mut offset = 0;
    let mut skipped = 0;
    while offset < plain.len() {
        let span = AtomSpan::read(&plain[offset..], offset as u64, Some(plain.len() as u64))
            .unwrap()
            .unwrap();
        let end = span.end.unwrap();
        if matches!(span.kind, AtomKind::Padding | AtomKind::Other) {
            skipped += end - span.offset;
        }
        offset = end as usize;
    }
    assert!(skipped > 0);
    let opts = PacketizerOptions {
        index: IndexOptions {
            object_len: Some(plain.len() as u64),
            ..Default::default()
        },
        max_skipped_metadata_bytes: skipped,
    };
    assert!(run_options(&plain, 137, None, opts).is_ok());
    assert!(
        run_options(
            &plain,
            137,
            None,
            PacketizerOptions {
                max_skipped_metadata_bytes: skipped - 1,
                ..opts
            }
        )
        .is_err()
    );
}

#[test]
fn index_and_range_resume_failures_are_sticky_and_preserve_request_identity() {
    let tail = index::file("1-tail.m4a");
    let mut p = Packetizer::new(PacketizerOptions {
        index: IndexOptions {
            object_len: Some(tail.len() as u64),
            ..Default::default()
        },
        ..Default::default()
    });
    let mut offset = 0;
    loop {
        match p.next(&tail[offset..], true).unwrap() {
            PacketStep::Discard(n) => offset += n,
            PacketStep::NeedIndex(_) => break,
            _ => panic!("no index request"),
        }
    }
    let wrong = AudioIndex::from_moov(
        index::moov(&tail),
        IndexOptions {
            object_len: Some(tail.len() as u64 + 1),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(p.install_index(wrong).is_err());
    assert!(p.next(&tail[offset..], true).is_err());
    let data = index::file("1-reorder.m4a");
    let mut p = Packetizer::new(PacketizerOptions {
        index: IndexOptions {
            object_len: Some(data.len() as u64),
            ..Default::default()
        },
        ..Default::default()
    });
    let mut offset = 0;
    loop {
        match p.next(&data[offset..], true).unwrap() {
            PacketStep::Discard(n) | PacketStep::Packet { consumed: n, .. } => offset += n,
            PacketStep::ReadAt(packet) => {
                let span = media(&data, packet.offset, packet.size).unwrap();
                assert!(
                    p.resume_at(
                        AtomSpan {
                            kind: AtomKind::FileType,
                            ..span
                        },
                        packet.offset
                    )
                    .is_err()
                );
                break;
            }
            _ => panic!("no range request"),
        }
    }
    assert!(p.next(&[], false).is_err());
    let data = index::file("1-front.m4a");
    let mut p = Packetizer::new(PacketizerOptions {
        index: IndexOptions {
            object_len: Some(data.len() as u64 + 1),
            ..Default::default()
        },
        ..Default::default()
    });
    let mut offset = 0;
    loop {
        match p.next(&data[offset..], true) {
            Ok(PacketStep::Discard(n) | PacketStep::Packet { consumed: n, .. }) => offset += n,
            Err(_) => break,
            _ => panic!("known object tail accepted"),
        }
    }
    assert!(p.next(&[], false).is_err());
}

#[test]
fn first_valid_packet_does_not_wait_for_remaining_media() {
    let data = index::file("1-front.m4a");
    let mut idx = index::index(&data);
    idx.next_packet().unwrap().unwrap();
    let sample = idx.next_packet().unwrap().unwrap();
    let prefix = sample.offset as usize + sample.size as usize;
    assert!(prefix < data.len());
    let mut p = Packetizer::new(PacketizerOptions {
        index: IndexOptions {
            object_len: Some(data.len() as u64),
            ..Default::default()
        },
        ..Default::default()
    });
    let mut offset = 0;
    let mut decoded = None;
    loop {
        match p.next(&data[offset..prefix], false).unwrap() {
            PacketStep::Discard(n) => offset += n,
            PacketStep::Packet { consumed, packet } => {
                let dec = decoded.get_or_insert_with(|| {
                    AacDecoder::try_new(
                        p.track()
                            .unwrap()
                            .codec_params
                            .as_ref()
                            .unwrap()
                            .audio()
                            .unwrap(),
                        &AudioDecoderOptions::default().gapless(true),
                    )
                    .unwrap()
                });
                let block = dec.decode_ref(&packet).unwrap();
                offset += consumed;
                if block.samples_interleaved() > 0 {
                    break;
                }
            }
            _ => panic!("first valid AAC output waited for unavailable later bytes"),
        }
    }
    assert_eq!(offset, prefix);
}

#[test]
fn index_budget_rejects_announced_atom_before_buffering_its_body() {
    let data = index::file("1-metadata-front.m4a");
    let mut at = 0;
    let movie = loop {
        let span = AtomSpan::read(&data[at..], at as u64, Some(data.len() as u64))
            .unwrap()
            .unwrap();
        if span.kind == AtomKind::Movie {
            break span;
        }
        at = span.end.unwrap() as usize;
    };
    assert!(movie.end.unwrap() - movie.offset > 65536);
    let prefix = movie.body_offset as usize;
    let mut p = Packetizer::new(PacketizerOptions {
        index: IndexOptions {
            object_len: Some(data.len() as u64),
            max_index_bytes: 65536,
            ..Default::default()
        },
        ..Default::default()
    });
    let mut offset = 0;
    loop {
        match p.next(&data[offset..prefix], false) {
            Ok(PacketStep::Discard(n)) => offset += n,
            Err(_) => break,
            _ => panic!("oversized declared index was buffered or awaited"),
        }
    }
}
