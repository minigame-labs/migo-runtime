use std::io::Cursor;
use symphonia_codec_aac::AacDecoder;
use symphonia_core::{
    codecs::audio::{AudioDecoder, AudioDecoderOptions},
    common::Limit,
    formats::{FormatOptions, FormatReader, SeekMode, SeekTo},
    io::MediaSourceStream,
    meta::MetadataOptions,
    units::Timestamp,
};
use symphonia_format_isomp4::{
    IsoMp4Reader,
    streaming::{AtomKind, AtomSpan, AudioIndex, IndexOptions, validate_file_type},
};
#[global_allocator]
static ALLOC: migo_alloc_probe::CountingAllocator = migo_alloc_probe::CountingAllocator::system();
pub(crate) fn file(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../fixtures")
            .join(name),
    )
    .unwrap()
}
pub(crate) fn moov(data: &[u8]) -> &[u8] {
    let mut offset = 0;
    while offset < data.len() {
        let span = AtomSpan::read(&data[offset..], offset as u64, Some(data.len() as u64))
            .unwrap()
            .unwrap();
        let end = span.end.unwrap() as usize;
        if span.kind == AtomKind::Movie {
            return &data[offset..end];
        }
        if span.kind == AtomKind::FileType {
            validate_file_type(&data[offset..end]).unwrap();
        }
        offset = end;
    }
    panic!("no moov")
}
pub(crate) fn index(data: &[u8]) -> AudioIndex {
    AudioIndex::from_moov(
        moov(data),
        IndexOptions {
            object_len: Some(data.len() as u64),
            ..Default::default()
        },
    )
    .unwrap()
}
pub(crate) fn pull(data: &[u8]) -> IsoMp4Reader<'static> {
    let mut opts = FormatOptions::default();
    opts.metadata_options = MetadataOptions::default().limit_total_bytes(Limit::Maximum(0));
    IsoMp4Reader::try_new(
        MediaSourceStream::new(Box::new(Cursor::new(data.to_vec())), Default::default()),
        opts,
    )
    .unwrap()
}
pub(crate) fn codec(index: &AudioIndex) -> AacDecoder {
    AacDecoder::try_new(
        index
            .track()
            .codec_params
            .as_ref()
            .unwrap()
            .audio()
            .unwrap(),
        &AudioDecoderOptions::default().gapless(true),
    )
    .unwrap()
}
#[test]
fn real_front_tail_split_co64_and_reordered_chunks_keep_packet_and_pcm_timeline() {
    let mut cases = 0;
    for (n, frames, channels) in [(0, 17_usize, 1), (1, 9737, 2), (2, 240137, 1)] {
        let modes = if n == 0 {
            &["front", "tail"][..]
        } else {
            &["front", "tail", "split", "reorder", "co64"][..]
        };
        for mode in modes {
            let name = format!("{n}-{mode}.m4a");
            let data = file(&name);
            let mut idx = index(&data);
            let mut reader = pull(&data);
            assert_eq!(idx.track().num_frames, Some(frames as u64));
            let mut dec = codec(&idx);
            let mut other = AacDecoder::try_new(
                reader.tracks()[0]
                    .codec_params
                    .as_ref()
                    .unwrap()
                    .audio()
                    .unwrap(),
                &AudioDecoderOptions::default().gapless(true),
            )
            .unwrap();
            let mut descriptors = Vec::new();
            let mut pcm = Vec::new();
            let mut oracle = Vec::new();
            let mut valid = 0_u64;
            while let Some(sample) = idx.next_packet().unwrap() {
                let packet = reader.next_packet().unwrap().unwrap();
                let offset = sample.offset as usize;
                let end = offset + sample.size as usize;
                let borrowed = sample.packet(&data[offset..end]).unwrap();
                assert_eq!(borrowed.data.as_ptr(), data[offset..end].as_ptr());
                assert_eq!(borrowed.data, &*packet.data, "{name}");
                assert_eq!(
                    (sample.pts, sample.dur, sample.trim_start, sample.trim_end),
                    (packet.pts, packet.dur, packet.trim_start, packet.trim_end)
                );
                // Independent FFmpeg-encoder fixture invariant: one 1024-frame
                // priming block, fixed AAC-LC blocks, exact sample-rate edits.
                assert_eq!(
                    sample.pts.get(),
                    (descriptors.len() as i64 - 1) * 1024,
                    "{name}"
                );
                let start = sample.pts.get();
                let expected = ((start + 1024).min(frames as i64) - start.max(0)).max(0) as u64;
                assert_eq!(sample.dur.get(), expected, "{name}");
                valid += expected;
                let block = dec.decode_ref(&borrowed).unwrap();
                let base = pcm.len();
                pcm.resize(base + block.samples_interleaved(), 0.);
                block.copy_to_slice_interleaved(&mut pcm[base..]);
                let block = other.decode(&packet).unwrap();
                let base = oracle.len();
                oracle.resize(base + block.samples_interleaved(), 0.);
                block.copy_to_slice_interleaved(&mut oracle[base..]);
                descriptors.push(sample);
            }
            assert_eq!(valid, frames as u64);
            assert_eq!(pcm.len(), frames * channels);
            assert_eq!(pcm, oracle, "{name}");
            assert!(reader.next_packet().unwrap().is_none());
            assert!(idx.next_packet().unwrap().is_none());
            assert_eq!(idx.packet_count() as usize, descriptors.len());
            idx.rewind().unwrap();
            for sample in descriptors {
                assert_eq!(idx.next_packet().unwrap(), Some(sample));
            }
            assert!(idx.next_packet().unwrap().is_none());
            cases += 1;
        }
    }
    assert_eq!(cases, 12);
}

#[test]
fn headers_are_bounded_transactional_and_preserve_absolute_64bit_extents() {
    let mut large = Vec::new();
    large.extend_from_slice(&1_u32.to_be_bytes());
    large.extend_from_slice(b"mdat");
    large.extend_from_slice(&32_u64.to_be_bytes());
    let offset = (1_u64 << 32) + 137;
    for n in 0..16 {
        assert!(AtomSpan::read(&large[..n], offset, None).unwrap().is_none());
    }
    let span = AtomSpan::read(&large, offset, None).unwrap().unwrap();
    assert_eq!(span.kind, AtomKind::MediaData);
    assert_eq!(span.header_len, 16);
    assert_eq!(span.body_offset, offset + 16);
    assert_eq!(span.end, Some(offset + 32));
    assert!(AtomSpan::read(&large, offset, Some(offset + 31)).is_err());
    assert!(AtomSpan::read(&large, u64::MAX - 15, None).is_err());
    let open = b"\0\0\0\0mdat";
    assert_eq!(AtomSpan::read(open, 0, None).unwrap().unwrap().end, None);
    assert_eq!(
        AtomSpan::read(open, 8, Some(100)).unwrap().unwrap().end,
        Some(100)
    );
    assert!(AtomSpan::read(open, 8, Some(15)).is_err());
    let mut bad = large.clone();
    bad[8..].copy_from_slice(&15_u64.to_be_bytes());
    assert!(AtomSpan::read(&bad, 0, None).is_err());
    let before = migo_alloc_probe::thread_counts().allocation_events();
    for _ in 0..256 {
        for n in 0..=16 {
            std::hint::black_box(AtomSpan::read(&large[..n], offset, None).unwrap());
        }
    }
    assert_eq!(
        migo_alloc_probe::thread_counts().allocation_events() - before,
        0
    );
}

#[test]
fn exact_index_packet_limits_partial_atoms_and_sticky_bad_extents_are_checked() {
    let data = file("1-front.m4a");
    let movie = moov(&data);
    assert!(
        AudioIndex::from_moov(
            movie,
            IndexOptions {
                max_index_bytes: movie.len() - 1,
                ..Default::default()
            }
        )
        .is_err()
    );
    assert!(
        AudioIndex::from_moov(
            movie,
            IndexOptions {
                max_index_bytes: movie.len(),
                ..Default::default()
            }
        )
        .is_ok()
    );
    assert!(
        AudioIndex::from_moov(
            movie,
            IndexOptions {
                max_packet_bytes: 1,
                ..Default::default()
            }
        )
        .is_err()
    );
    for n in (0..16).chain(movie.len() - 16..movie.len()) {
        assert!(AudioIndex::from_moov(&movie[..n], Default::default()).is_err());
    }
    let mut extra = movie.to_vec();
    extra.push(0);
    assert!(AudioIndex::from_moov(&extra, Default::default()).is_err());
    let mut open = movie.to_vec();
    open[..4].fill(0);
    assert!(AudioIndex::from_moov(&open, Default::default()).is_err());
    let mut idx = AudioIndex::from_moov(
        movie,
        IndexOptions {
            object_len: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(idx.next_packet().is_err());
    assert!(idx.next_packet().is_err());
    assert!(idx.rewind().is_err());
    let data = file("1-co64.m4a");
    let mut movie = moov(&data).to_vec();
    let table = movie.windows(4).position(|b| b == b"co64").unwrap();
    movie[table + 12..table + 20].copy_from_slice(&(u64::MAX - 1).to_be_bytes());
    let mut idx = AudioIndex::from_moov(&movie, Default::default()).unwrap();
    assert!(idx.next_packet().is_err());
    assert!(idx.next_packet().is_err());
}

#[test]
fn indexed_packets_decode_and_convert_without_steady_rust_allocation() {
    let data = file("2-split.m4a");
    let mut idx = index(&data);
    let mut dec = codec(&idx);
    let mut scratch = vec![0.; 2048];
    // The first packet is priming and may decode to zero samples.
    let sample = idx.next_packet().unwrap().unwrap();
    let start = sample.offset as usize;
    dec.decode_ref(
        &sample
            .packet(&data[start..start + sample.size as usize])
            .unwrap(),
    )
    .unwrap();
    let mut step = || {
        let sample = idx.next_packet().unwrap().unwrap();
        let start = sample.offset as usize;
        let block = dec
            .decode_ref(
                &sample
                    .packet(&data[start..start + sample.size as usize])
                    .unwrap(),
            )
            .unwrap();
        let count = block.samples_interleaved();
        assert!(count > 0);
        block.copy_to_slice_interleaved(&mut scratch[..count]);
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
fn unused_metadata_is_skipped_before_value_allocation() {
    let clean = file("1-tail.m4a");
    let large = file("1-metadata.m4a");
    let parse = |data: &[u8]| {
        let movie = moov(data);
        let before = migo_alloc_probe::thread_counts();
        let parsed = AudioIndex::from_moov(movie, Default::default()).unwrap();
        let after = migo_alloc_probe::thread_counts();
        (parsed, after.bytes_allocated - before.bytes_allocated)
    };
    let (mut a, plain_bytes) = parse(&clean);
    let (mut b, tagged_bytes) = parse(&large);
    assert!(
        tagged_bytes <= plain_bytes + 65536,
        "unused 1MiB value was allocated: {plain_bytes}/{tagged_bytes}"
    );
    assert_eq!(a.packet_count(), b.packet_count());
    while let Some(left) = a.next_packet().unwrap() {
        assert_eq!(b.next_packet().unwrap(), Some(left));
    }
    assert!(b.next_packet().unwrap().is_none());
    println!("M4A_INDEX_METADATA_ALLOCATED_BYTES plain={plain_bytes} tagged={tagged_bytes}");
}

#[test]
fn inconsistent_counts_and_multiple_tracks_are_rejected_by_the_selected_profile() {
    let data = file("1-front.m4a");
    let mut movie = moov(&data).to_vec();
    let sizes = movie.windows(4).position(|p| p == b"stsz").unwrap();
    let chunks = movie.windows(4).position(|p| p == b"stsc").unwrap();
    let count = u32::from_be_bytes(movie[sizes + 12..sizes + 16].try_into().unwrap());
    assert!(count > 1);
    movie[sizes + 12..sizes + 16].copy_from_slice(&(count - 1).to_be_bytes());
    // One track/one chunk fixture: leave chunk count consistent with stsz so
    // the independent stts count guard, rather than another guard, is tested.
    movie[chunks + 16..chunks + 20].copy_from_slice(&(count - 1).to_be_bytes());
    assert!(AudioIndex::from_moov(&movie, Default::default()).is_err());
    let movie = moov(&data);
    let mut offset = 8;
    let mut track = None;
    while offset < movie.len() {
        let span = AtomSpan::read(&movie[offset..], offset as u64, Some(movie.len() as u64))
            .unwrap()
            .unwrap();
        let end = span.end.unwrap() as usize;
        if &movie[offset + 4..offset + 8] == b"trak" {
            track = Some(movie[offset..end].to_vec());
        }
        offset = end;
    }
    let mut two = movie.to_vec();
    two.extend_from_slice(&track.unwrap());
    let size = two.len() as u32;
    two[..4].copy_from_slice(&size.to_be_bytes());
    assert!(AudioIndex::from_moov(&two, Default::default()).is_err());
}

#[test]
fn co64_sample_positions_keep_the_full_absolute_offset() {
    let data = file("1-co64.m4a");
    let mut movie = moov(&data).to_vec();
    let table = movie.windows(4).position(|p| p == b"co64").unwrap();
    let offset = (1_u64 << 32) + 137;
    movie[table + 12..table + 20].copy_from_slice(&offset.to_be_bytes());
    // This tests the index descriptor itself. It is not a claim that an
    // actual 4GiB HTTP representation has been fetched or validated.
    let mut idx = AudioIndex::from_moov(&movie, Default::default()).unwrap();
    assert_eq!(idx.next_packet().unwrap().unwrap().offset, offset);
}

fn targets(frames: usize) -> Vec<usize> {
    let mut targets = vec![frames - 1, 0, 1, frames / 2];
    targets.extend(
        [211, 1023, 1024, 2100]
            .into_iter()
            .filter(|&frame| frame < frames),
    );
    targets
}

fn seek_files(deterministic: bool) -> Vec<String> {
    let mut names = Vec::new();
    for number in 0..3 {
        for mode in if number == 0 {
            &["front", "tail"][..]
        } else {
            &["front", "tail", "split", "reorder", "co64"][..]
        } {
            names.push(format!(
                "{number}-{}{mode}.m4a",
                if deterministic { "seek-" } else { "" }
            ));
        }
    }
    names.extend(["1-seek-scaled.m4a".into(), "1-seek-edit.m4a".into()]);
    names
}

fn decode_index_tail(
    data: &[u8],
    idx: &mut AudioIndex,
    decoder: &mut AacDecoder,
    target: usize,
) -> (Vec<f32>, usize) {
    let channels = decoder.codec_params().channels.as_ref().unwrap().count();
    let mut pcm = Vec::new();
    let mut packets = 0;
    while let Some(sample) = idx.next_packet().unwrap() {
        let at = sample.offset as usize;
        let packet = sample.packet(&data[at..at + sample.size as usize]).unwrap();
        let start = sample.pts.get() + sample.trim_start.get() as i64;
        let block = decoder.decode_ref(&packet).unwrap();
        let mut scratch = vec![0.0; block.samples_interleaved()];
        block.copy_to_slice_interleaved(&mut scratch);
        let skip = (target as i64 - start).max(0) as usize;
        pcm.extend_from_slice(&scratch[(skip * channels).min(scratch.len())..]);
        packets += 1;
    }
    (pcm, packets)
}

#[test]
fn index_seek_matches_pull_descriptors_and_full_packet_preroll_after_eof() {
    let mut comparisons = 0;
    for name in seek_files(false) {
        let data = file(&name);
        let mut idx = index(&data);
        let frames = idx.track().num_frames.unwrap() as usize;
        let mut full = Vec::new();
        while let Some(packet) = idx.next_packet().unwrap() {
            full.push(packet);
        }
        let mut reader = pull(&data);
        for target in targets(frames) {
            let requested = Timestamp::new(target as i64);
            let seeked = idx.seek(requested).unwrap();
            let expected = full
                .iter()
                .rev()
                .find(|packet| packet.pts.get() <= target as i64 - 1024)
                .unwrap_or(&full[0]);
            assert_eq!(seeked.required_ts, requested);
            assert_eq!(seeked.actual_ts, expected.pts);
            let other = reader
                .seek(
                    SeekMode::Accurate,
                    SeekTo::Timestamp {
                        ts: requested,
                        track_id: idx.track().id,
                    },
                )
                .unwrap();
            assert_eq!(
                (seeked.required_ts, seeked.actual_ts),
                (other.required_ts, other.actual_ts)
            );
            assert_eq!(seeked.track_id, idx.track().id);
            let first = idx.next_packet().unwrap().unwrap();
            assert_eq!(&first, expected);
            let packet = reader.next_packet().unwrap().unwrap();
            assert_eq!(first.pts, packet.pts);
            assert_eq!(first.dur, packet.dur);
            assert_eq!(
                (first.trim_start, first.trim_end),
                (packet.trim_start, packet.trim_end)
            );
            let at = first.offset as usize;
            assert_eq!(&data[at..at + first.size as usize], &*packet.data);
            comparisons += 1;
        }
    }
    assert_eq!(comparisons, 104);
    println!("W09_INDEX_SEEK_DESCRIPTORS {comparisons} exact comparisons");
}

#[test]
fn deterministic_aac_seek_preroll_matches_full_pcm_suffix() {
    let mut comparisons = 0;
    for name in seek_files(true) {
        let data = file(&name);
        let mut idx = index(&data);
        let frames = idx.track().num_frames.unwrap() as usize;
        let mut decoder = codec(&idx);
        let mut reader = pull(&data);
        let mut other = codec(&idx);
        let channels = decoder.codec_params().channels.as_ref().unwrap().count();
        let (full, _) = decode_index_tail(&data, &mut idx, &mut decoder, 0);
        assert_eq!(full.len(), frames * channels);
        for target in targets(frames) {
            let seeked = idx.seek(Timestamp::new(target as i64)).unwrap();
            assert_eq!(seeked.required_ts.get(), target as i64);
            decoder.reset();
            let (tail, packets) = decode_index_tail(&data, &mut idx, &mut decoder, target);
            assert_eq!(tail.len(), (frames - target) * channels);
            reader
                .seek(
                    SeekMode::Accurate,
                    SeekTo::Timestamp {
                        ts: Timestamp::new(target as i64),
                        track_id: idx.track().id,
                    },
                )
                .unwrap();
            other.reset();
            let mut pulled = Vec::new();
            while let Some(packet) = reader.next_packet().unwrap() {
                let start = packet.pts.get() + packet.trim_start.get() as i64;
                let block = other.decode(&packet).unwrap();
                let mut scratch = vec![0.0; block.samples_interleaved()];
                block.copy_to_slice_interleaved(&mut scratch);
                let skip = (target as i64 - start).max(0) as usize;
                pulled.extend_from_slice(&scratch[(skip * channels).min(scratch.len())..]);
            }
            assert_eq!(tail, pulled, "index/pull PCM differs at {name}/{target}");
            let max_error = tail
                .iter()
                .zip(&full[target * channels..])
                .map(|(a, b)| (a - b).abs())
                .fold(0.0_f32, f32::max);
            assert!(max_error < 0.000_01, "{name}/{target}: {max_error}");
            if target == frames - 1 {
                assert!(packets <= 3, "seek decoded the prefix again");
            }
            comparisons += 1;
        }
    }
    assert_eq!(comparisons, 104);
    println!("W09_INDEX_SEEK_PCM {comparisons} suffixes, max abs error < 0.00001 (PNS disabled)");
}

#[test]
fn invalid_seek_preserves_cursor_and_corrupt_extents_remain_failed() {
    let data = file("1-front.m4a");
    let mut idx = index(&data);
    let mut expected = index(&data);
    idx.next_packet().unwrap();
    expected.next_packet().unwrap();
    for value in [-1, 9737, i64::MAX] {
        assert!(idx.seek(Timestamp::new(value)).is_err());
        assert_eq!(idx.next_packet().unwrap(), expected.next_packet().unwrap());
    }
    let mut short = AudioIndex::from_moov(
        moov(&data),
        IndexOptions {
            object_len: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(short.seek(Timestamp::ZERO).is_err());
    assert!(short.next_packet().is_err());
    assert!(short.seek(Timestamp::ZERO).is_err());
    let data = file("1-co64.m4a");
    let mut movie = moov(&data).to_vec();
    let table = movie.windows(4).position(|p| p == b"co64").unwrap();
    movie[table + 12..table + 20].copy_from_slice(&(u64::MAX - 1).to_be_bytes());
    let mut idx = AudioIndex::from_moov(&movie, Default::default()).unwrap();
    assert!(idx.seek(Timestamp::ZERO).is_err());
    assert!(
        idx.seek(Timestamp::new(9000)).is_err(),
        "corrupt index revived at a valid later chunk"
    );
    assert!(idx.next_packet().is_err());
    assert!(idx.rewind().is_err());
}

#[test]
fn index_seek_and_first_descriptor_have_no_steady_heap_activity() {
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    let data = file("2-split.m4a");
    let mut idx = index(&data);
    assert_no_steady_state_allocation(
        Burst {
            path: "index presentation seek and first borrowed descriptor",
            warmup: 8,
            measured: 256,
        },
        |tick| {
            let requested = Timestamp::new(if tick % 2 == 0 { 200000 } else { 17 });
            let seeked = idx.seek(requested).unwrap();
            assert_eq!(seeked.required_ts, requested);
            let packet = idx.next_packet().unwrap().unwrap();
            assert_eq!(packet.pts, seeked.actual_ts);
        },
    );
}

#[test]
fn co64_seek_preserves_offsets_above_4gib_and_preroll_sample_positions() {
    let data = file("1-co64.m4a");
    let mut movie = moov(&data).to_vec();
    let table = movie.windows(4).position(|p| p == b"co64").unwrap();
    let old = u64::from_be_bytes(movie[table + 12..table + 20].try_into().unwrap());
    let base = (1_u64 << 32) + 137;
    let count = u32::from_be_bytes(movie[table + 8..table + 12].try_into().unwrap());
    for entry in 0..count as usize {
        let at = table + 12 + entry * 8;
        let offset = u64::from_be_bytes(movie[at..at + 8].try_into().unwrap());
        movie[at..at + 8].copy_from_slice(&(base + offset - old).to_be_bytes());
    }
    let mut shifted = AudioIndex::from_moov(&movie, Default::default()).unwrap();
    let mut original = index(&data);
    for target in [9000, 0, 5000, 1] {
        shifted.seek(Timestamp::new(target)).unwrap();
        original.seek(Timestamp::new(target)).unwrap();
        for _ in 0..2 {
            let a = shifted.next_packet().unwrap().unwrap();
            let b = original.next_packet().unwrap().unwrap();
            assert_eq!(a.offset, base + b.offset - old);
            assert_eq!((a.pts, a.size), (b.pts, b.size));
        }
    }
}

#[test]
fn ordinary_pns_aac_seek_keeps_valid_pcm_and_exact_presentation_counts() {
    let mut comparisons = 0;
    for number in [1, 2] {
        let data = file(&format!("{number}-reorder.m4a"));
        let mut idx = index(&data);
        let mut decoder = codec(&idx);
        let channels = decoder.codec_params().channels.as_ref().unwrap().count();
        let frames = idx.track().num_frames.unwrap() as usize;
        for target in targets(frames) {
            idx.seek(Timestamp::new(target as i64)).unwrap();
            decoder.reset();
            let (pcm, _) = decode_index_tail(&data, &mut idx, &mut decoder, target);
            assert_eq!(pcm.len(), (frames - target) * channels);
            assert!(
                pcm.iter()
                    .all(|sample| sample.is_finite() && sample.abs() < 2.0)
            );
            comparisons += 1;
        }
    }
    assert_eq!(comparisons, 16);
    println!(
        "W09_INDEX_SEEK_PNS {comparisons} exact frame counts; noise realization may differ after reset"
    );
}
