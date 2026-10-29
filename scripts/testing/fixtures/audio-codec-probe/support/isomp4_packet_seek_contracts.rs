//! Drive the library's real seek/ReadAt/window protocol; no replacement parser.
use super::*;
use symphonia_core::units::Timestamp;
use symphonia_format_isomp4::streaming::BufferedStep;

fn prepare(data: &[u8], finished: bool, quota: u64) -> Packetizer {
    let mut parser = Packetizer::new(PacketizerOptions {
        index: IndexOptions {
            object_len: Some(data.len() as u64),
            ..Default::default()
        },
        max_skipped_metadata_bytes: quota,
    });
    let mut at = 0;
    for _ in 0..data.len() + 1024 {
        match parser.next(&data[at..], true).unwrap() {
            PacketStep::NeedIndex(_) => parser.install_index(index::index(data)).unwrap(),
            PacketStep::ReadAt(packet) => {
                parser
                    .resume_at(
                        media(data, packet.offset, packet.size).unwrap(),
                        packet.offset,
                    )
                    .unwrap();
                at = packet.offset as usize;
            }
            PacketStep::Discard(n) => at += n,
            PacketStep::Packet { consumed, .. } => {
                at += consumed;
                if !finished {
                    return parser;
                }
            }
            PacketStep::End => {
                assert_eq!(at, data.len());
                return parser;
            }
            PacketStep::NeedMore => panic!("complete preparation requested bytes"),
        }
    }
    panic!("packet preparation made no bounded progress");
}

fn request(parser: &mut Packetizer) -> symphonia_format_isomp4::streaming::IndexedPacket {
    let before = parser.position();
    let first = match parser.next(b"old-unverified-input", true).unwrap() {
        PacketStep::ReadAt(packet) => packet,
        _ => panic!("seek did not suspend for verified source bytes"),
    };
    assert_eq!(parser.position(), before);
    for _ in 0..3 {
        match parser.next(&[], false).unwrap() {
            PacketStep::ReadAt(again) => assert_eq!(again, first),
            _ => panic!("pending seek request changed"),
        }
        assert_eq!(parser.position(), before);
    }
    first
}

fn decode_tail(
    parser: &mut Packetizer,
    data: &[u8],
    decoder: &mut AacDecoder,
    target: usize,
    fragment: usize,
) -> Result<(Vec<f32>, usize), String> {
    let mut at = parser.position() as usize;
    let mut available = at;
    let mut pcm = Vec::new();
    let mut packets = 0;
    let channels = decoder.codec_params().channels.as_ref().unwrap().count();
    for _ in 0..data.len() * 4 + 1024 {
        match parser
            .next(&data[at..available], available == data.len())
            .map_err(|e| e.to_string())?
        {
            PacketStep::ReadAt(packet) => {
                parser
                    .resume_at(media(data, packet.offset, packet.size)?, packet.offset)
                    .map_err(|e| e.to_string())?;
                at = packet.offset as usize;
                available = at;
            }
            PacketStep::NeedMore => {
                if available == data.len() {
                    return Err("seek requested input beyond EOF".into());
                }
                available = (available + fragment).min(data.len());
            }
            PacketStep::Discard(n) => at += n,
            PacketStep::Packet { consumed, packet } => {
                assert_eq!(packet.data.as_ptr(), data[at..].as_ptr());
                let start = packet.pts.get() + packet.trim_start.get() as i64;
                let block = decoder.decode_ref(&packet).map_err(|e| e.to_string())?;
                let mut scratch = vec![0.0; block.samples_interleaved()];
                block.copy_to_slice_interleaved(&mut scratch);
                let skip = (target as i64 - start).max(0) as usize * channels;
                pcm.extend_from_slice(&scratch[skip.min(scratch.len())..]);
                at += consumed;
                packets += 1;
            }
            PacketStep::End => {
                assert_eq!(at, data.len());
                return Ok((pcm, packets));
            }
            PacketStep::NeedIndex(_) => {
                return Err("seek attempted to reparse its retained index".into());
            }
        }
    }
    Err("seek made no bounded progress".into())
}

#[test]
fn seek_after_eof_retains_index_and_recovers_the_exact_aac_suffix() {
    let mut comparisons = 0;
    for name in index::seek_files(true) {
        let data = index::file(&name);
        let idx = index::index(&data);
        let frames = idx.track().num_frames.unwrap() as usize;
        let mut decoder = index::codec(&idx);
        let channels = decoder.codec_params().channels.as_ref().unwrap().count();
        let full = run(&data, 100000, Some(data.len() as u64), None).unwrap().0;
        for fragment in [137, 100000] {
            let mut parser = prepare(&data, true, 4 * 1024 * 1024);
            for target in index::targets(frames) {
                let old = parser.position();
                let seeked = parser.seek(Timestamp::new(target as i64)).unwrap();
                assert_eq!(seeked.required_ts.get(), target as i64);
                assert_eq!(parser.position(), old);
                let packet = request(&mut parser);
                assert_eq!(packet.pts, seeked.actual_ts);
                decoder.reset();
                let (tail, packets) =
                    decode_tail(&mut parser, &data, &mut decoder, target, fragment).unwrap();
                assert_eq!(tail.len(), (frames - target) * channels);
                let error = tail
                    .iter()
                    .zip(&full[target * channels..])
                    .map(|(a, b)| (a - b).abs())
                    .fold(0.0_f32, f32::max);
                assert!(error < 0.000_01, "{name}/{fragment}/{target}: {error}");
                if target == frames - 1 {
                    assert!(packets <= 3);
                }
                comparisons += 1;
            }
        }
    }
    assert_eq!(comparisons, 208);
    println!("W09_PACKET_SEEK_PCM {comparisons} suffixes, abs error < 0.00001 (PNS disabled)");
}

#[test]
fn seek_wait_rejects_unverified_offsets_kinds_and_extents() {
    let data = index::file("1-seek-front.m4a");
    for violation in 0..4 {
        let mut parser = prepare(&data, false, 4 * 1024 * 1024);
        parser.seek(Timestamp::new(5000)).unwrap();
        let packet = request(&mut parser);
        let mut span = media(&data, packet.offset, packet.size).unwrap();
        let mut offset = packet.offset;
        match violation {
            0 => offset += 1,
            1 => span.kind = AtomKind::Padding,
            2 => span.end = Some(packet.offset + u64::from(packet.size) - 1),
            _ => span.end = Some(data.len() as u64 + 1),
        }
        assert!(parser.resume_at(span, offset).is_err());
        assert!(parser.seek(Timestamp::ZERO).is_err());
        assert!(parser.next(&[], true).is_err());
    }
}

#[test]
fn cached_complete_and_partial_windows_resolve_seek_without_external_io() {
    for name in ["1-seek-front.m4a", "1-seek-tail.m4a", "1-seek-reorder.m4a"] {
        let data = index::file(name);
        let mut parser = prepare(&data, true, 4 * 1024 * 1024);
        for target in [5000, 0, 9000] {
            parser.seek(Timestamp::new(target)).unwrap();
            let packet = request(&mut parser);
            let resolved = parser.try_buffered(0, &data, 65536).unwrap();
            assert_eq!(
                resolved,
                BufferedStep::ReadAt {
                    offset: packet.offset
                }
            );
            assert_eq!(parser.position(), packet.offset);
        }
    }
    let data = index::file("1-seek-front.m4a");
    let mut parser = prepare(&data, false, 4 * 1024 * 1024);
    let before = parser.position();
    parser.seek(Timestamp::new(5000)).unwrap();
    let packet = request(&mut parser);
    assert!(
        packet.offset > before,
        "explicit forward ReadAt must also resolve"
    );
    let span = media(&data, packet.offset, packet.size).unwrap();
    let base = span.body_offset;
    assert!(base > 0);
    assert_eq!(
        parser
            .try_buffered(
                base,
                &data[base as usize..span.end.unwrap() as usize],
                65536
            )
            .unwrap(),
        BufferedStep::ReadAt {
            offset: packet.offset
        }
    );
}

#[test]
fn replay_reuses_tail_movie_and_resets_each_traversal_metadata_budget() {
    let mut data = index::file("1-seek-tail.m4a");
    data.extend_from_slice(&(32768_u32 + 8).to_be_bytes());
    data.extend_from_slice(b"free");
    data.resize(data.len() + 32768, 0);
    let mut parser = prepare(&data, true, 65536);
    let mut decoder = index::codec(&index::index(&data));
    for _ in 0..4 {
        parser.seek(Timestamp::ZERO).unwrap();
        request(&mut parser);
        decoder.reset();
        let (pcm, _) = decode_tail(&mut parser, &data, &mut decoder, 0, 100000).unwrap();
        assert_eq!(pcm.len(), 9737 * 2);
    }
}

#[test]
fn invalid_targets_preserve_requests_and_replay_rejects_foreign_movie() {
    let data = index::file("1-seek-front.m4a");
    let mut parser = prepare(&data, true, 4 * 1024 * 1024);
    parser.seek(Timestamp::new(5000)).unwrap();
    let packet = request(&mut parser);
    for target in [-1, 9737, i64::MAX] {
        assert!(parser.seek(Timestamp::new(target)).is_err());
        assert_eq!(request(&mut parser), packet);
    }
    let mut empty = Packetizer::new(Default::default());
    assert!(empty.seek(Timestamp::ZERO).is_err());
    assert_eq!(empty.position(), 0);
    assert!(matches!(
        empty.next(&[], false).unwrap(),
        PacketStep::NeedMore
    ));
    let movie = index::moov(&data).to_vec();
    let mut foreign = data;
    foreign.extend_from_slice(&movie);
    let mut parser = prepare(&foreign, false, 4 * 1024 * 1024);
    parser.seek(Timestamp::ZERO).unwrap();
    let mut decoder = index::codec(&index::index(&foreign));
    assert!(decode_tail(&mut parser, &foreign, &mut decoder, 0, 100000).is_err());
    assert!(parser.seek(Timestamp::ZERO).is_err());
}

#[test]
fn seek_request_and_cached_resume_have_no_steady_heap_activity() {
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    let data = index::file("1-seek-front.m4a");
    let mut parser = prepare(&data, true, 4 * 1024 * 1024);
    assert_no_steady_state_allocation(
        Burst {
            path: "packetizer seek/read-at/cached resume",
            warmup: 8,
            measured: 256,
        },
        |tick| {
            parser
                .seek(Timestamp::new(if tick % 2 == 0 { 5000 } else { 0 }))
                .unwrap();
            let packet = request(&mut parser);
            assert_eq!(
                parser.try_buffered(0, &data, 65536).unwrap(),
                BufferedStep::ReadAt {
                    offset: packet.offset
                }
            );
            let offset = packet.offset as usize;
            match parser.next(&data[offset..], true).unwrap() {
                PacketStep::Packet {
                    packet: borrowed, ..
                } => assert_eq!(borrowed.data.as_ptr(), data[offset..].as_ptr()),
                _ => panic!("resolved seek did not return borrowed packet"),
            }
        },
    );
}
