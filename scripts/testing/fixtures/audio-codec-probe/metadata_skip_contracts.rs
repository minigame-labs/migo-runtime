//! Optional metadata is skipped in its owning library, before value decoding.
//! Container framing, codec setup and sample timelines remain observable.
use std::io::{Cursor, Seek, SeekFrom};
use symphonia::core::{
    common::Limit,
    formats::{FormatOptions, TrackType, probe::Hint},
    io::{BufReader, MediaSourceStream, ReadBytes, ReadOnlySource},
    meta::{MetadataOptions, MetadataReader},
};
use symphonia_metadata::{ape::ApeReader, id3v1::Id3v1Reader, id3v2::Id3v2Reader};

fn disabled() -> MetadataOptions {
    MetadataOptions::default().limit_total_bytes(Limit::Maximum(0))
}
fn source(data: &[u8], seekable: bool) -> MediaSourceStream<'_> {
    if seekable {
        MediaSourceStream::new(Box::new(Cursor::new(data)), Default::default())
    } else {
        MediaSourceStream::new(
            Box::new(ReadOnlySource::new(Cursor::new(data))),
            Default::default(),
        )
    }
}
fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::PathBuf::from(std::env::var_os("W09_METADATA_SKIP_FIXTURES").unwrap())
            .join(name),
    )
    .unwrap()
}
fn metadata(data: &[u8], options: MetadataOptions, seekable: bool) -> (usize, usize, usize, u32) {
    let mut reader = symphonia::default::get_probe()
        .probe(
            &Hint::new(),
            source(data, seekable),
            FormatOptions::default(),
            options,
        )
        .unwrap();
    let rate = reader
        .default_track(TrackType::Audio)
        .unwrap()
        .codec_params
        .as_ref()
        .unwrap()
        .audio()
        .unwrap()
        .sample_rate
        .unwrap();
    let mut log = reader.metadata();
    let (mut revisions, mut tags, mut visuals) = (0, 0, 0);
    loop {
        if let Some(rev) = log.current() {
            revisions += 1;
            tags += rev.media.tags.len();
            visuals += rev.media.visuals.len();
        }
        if log.pop().is_none() {
            break;
        }
    }
    (revisions, tags, visuals, rate)
}
fn id3v1() -> Vec<u8> {
    let mut tag = vec![b' '; 128];
    tag[..3].copy_from_slice(b"TAG");
    tag[3..7].copy_from_slice(b"kept");
    tag[127] = 0;
    tag
}
fn ape_item(key: &[u8], value: &[u8]) -> Vec<u8> {
    let mut item = (value.len() as u32).to_le_bytes().to_vec();
    item.extend(0u32.to_le_bytes());
    item.extend_from_slice(key);
    item.push(0);
    item.extend_from_slice(value);
    item
}
fn ape_preamble(version: u32, size: u32, flags: u32) -> Vec<u8> {
    let mut result = b"APETAGEX".to_vec();
    for value in [version, size, 1, flags, 0, 0] {
        result.extend(value.to_le_bytes());
    }
    assert_eq!(result.len(), 32);
    result
}
fn ape(value: &[u8], footer: bool) -> Vec<u8> {
    let item = ape_item(b"Title", value);
    let size = item.len() as u32 + if footer { 32 } else { 0 };
    let flags = 0x8000_0000 | if footer { 0 } else { 0x4000_0000 };
    let mut tag = ape_preamble(2000, size, flags | 0x2000_0000);
    tag.extend(item);
    if footer {
        tag.extend(ape_preamble(2000, size, flags));
    }
    tag
}
fn compressed_id3v22(size: usize) -> Vec<u8> {
    let mut data = b"ID3\x02\0\x40".to_vec();
    for shift in [21, 14, 7, 0] {
        data.push(((size >> shift) & 127) as u8);
    }
    data.resize(10 + size, 0xa5);
    data
}
fn chunk(tag: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut result = tag.to_vec();
    result.extend((data.len() as u32).to_le_bytes());
    result.extend(data);
    if data.len() % 2 != 0 {
        result.push(0);
    }
    result
}
fn wave(info: &[u8]) -> Vec<u8> {
    let mut body = b"WAVE".to_vec();
    let mut format = Vec::new();
    for value in [1u16, 1] {
        format.extend(value.to_le_bytes());
    }
    for value in [8000u32, 16000] {
        format.extend(value.to_le_bytes());
    }
    for value in [2u16, 16] {
        format.extend(value.to_le_bytes());
    }
    body.extend(chunk(b"fmt ", &format));
    if !info.is_empty() {
        let mut list = b"INFO".to_vec();
        list.extend(chunk(b"INAM", info));
        body.extend(chunk(b"LIST", &list));
    }
    body.extend(chunk(b"data", &[0u8; 128]));
    chunk(b"RIFF", &body)
}

#[test]
fn id3v1_zero_retention_preserves_the_following_media() {
    let mut data = id3v1();
    data.extend(b"MEDIA");
    let mut reader = Id3v1Reader::try_new(source(&data, true), disabled()).unwrap();
    let result = reader.read_all().unwrap();
    assert!(result.revision.media.tags.is_empty());
    let mut inner = Box::new(reader).into_inner();
    assert_eq!(inner.read_boxed_slice_exact(5).unwrap().as_ref(), b"MEDIA");
}
#[test]
fn id3v1_default_still_retains_tags() {
    assert!(
        !Id3v1Reader::try_new(source(&id3v1(), true), Default::default())
            .unwrap()
            .read_all()
            .unwrap()
            .revision
            .media
            .tags
            .is_empty()
    );
}
#[test]
fn ape_zero_retention_skips_before_large_value_allocation() {
    let mut data = ape(&vec![b'x'; 2 * 1024 * 1024], true);
    data.extend(b"MEDIA");
    for from_footer in [false, true] {
        let mut input = source(&data, true);
        if from_footer {
            input
                .seek(SeekFrom::Start((data.len() - 5 - 32) as u64))
                .unwrap();
        }
        let mut reader = ApeReader::try_new(input, disabled()).unwrap();
        let before = migo_alloc_probe::thread_counts().bytes_allocated;
        let result = reader.read_all().unwrap();
        let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before;
        assert!(result.revision.media.tags.is_empty());
        assert!(
            allocated < 32 * 1024,
            "APE decoded unused data: {allocated}"
        );
        assert_eq!(
            Box::new(reader)
                .into_inner()
                .read_boxed_slice_exact(5)
                .unwrap()
                .as_ref(),
            b"MEDIA"
        );
    }
}
#[test]
fn ape_header_only_tag_does_not_consume_following_media() {
    for options in [MetadataOptions::default(), disabled()] {
        let mut data = ape(b"kept", false);
        data.extend(b"MEDIA");
        let mut reader = ApeReader::try_new(source(&data, false), options).unwrap();
        let result = reader.read_all().unwrap();
        assert_eq!(
            result.revision.media.tags.is_empty(),
            matches!(options.limit_total_bytes, Limit::Maximum(0))
        );
        assert_eq!(
            Box::new(reader)
                .into_inner()
                .read_boxed_slice_exact(5)
                .unwrap()
                .as_ref(),
            b"MEDIA"
        );
    }
}
#[test]
fn ape_default_header_and_footer_positions_agree() {
    let data = ape(b"kept", true);
    for offset in [0, data.len() - 32] {
        let mut input = source(&data, true);
        input.seek(SeekFrom::Start(offset as u64)).unwrap();
        let result = ApeReader::try_new(input, Default::default())
            .unwrap()
            .read_all()
            .unwrap();
        assert_eq!(result.revision.media.tags[0].raw.value.to_string(), "kept");
    }
}
#[test]
fn ape_v1_footer_only_still_works() {
    let mut data = ape_item(b"Title", b"kept");
    let size = data.len() as u32 + 32;
    data.extend(ape_preamble(1000, size, 0));
    for options in [MetadataOptions::default(), disabled()] {
        let mut input = source(&data, true);
        input
            .seek(SeekFrom::Start((data.len() - 32) as u64))
            .unwrap();
        let result = ApeReader::try_new(input, options)
            .unwrap()
            .read_all()
            .unwrap();
        assert_eq!(
            result.revision.media.tags.is_empty(),
            matches!(options.limit_total_bytes, Limit::Maximum(0))
        );
    }
}
#[test]
fn ape_skip_still_checks_footer_identity_and_minimum_extent() {
    let mut mismatched = ape(b"kept", true);
    let offset = mismatched.len() - 32 + 16;
    mismatched[offset] = 2;
    assert!(
        ApeReader::try_new(source(&mismatched, true), disabled())
            .unwrap()
            .read_all()
            .is_err()
    );
    let invalid = ape_preamble(2000, 1, 0xa000_0000);
    assert!(
        ApeReader::try_new(source(&invalid, true), disabled())
            .unwrap()
            .read_all()
            .is_err()
    );
}
#[test]
fn ape_item_cannot_borrow_bytes_from_the_footer_or_media() {
    let mut data = ape(b"kept", true);
    data[32..36].copy_from_slice(&((128 * 1024) as u32).to_le_bytes());
    data.extend(vec![b'x'; 128 * 1024]);
    let mut reader = ApeReader::try_new(source(&data, true), Default::default()).unwrap();
    let before = migo_alloc_probe::thread_counts().bytes_allocated;
    assert!(reader.read_all().is_err());
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before;
    assert!(
        allocated < 32 * 1024,
        "APE item escaped its own extent: {allocated}"
    );
}
#[test]
fn compressed_id3v22_skip_uses_encoded_extent_without_an_inflater() {
    let mut data = compressed_id3v22(2 * 1024 * 1024);
    data.extend(b"MEDIA");
    let mut reader = Id3v2Reader::try_new(source(&data, false), disabled()).unwrap();
    let before = migo_alloc_probe::thread_counts().bytes_allocated;
    let result = reader.read_all().unwrap();
    assert!(result.revision.media.tags.is_empty());
    assert!(migo_alloc_probe::thread_counts().bytes_allocated - before < 4096);
    assert_eq!(
        Box::new(reader)
            .into_inner()
            .read_boxed_slice_exact(5)
            .unwrap()
            .as_ref(),
        b"MEDIA"
    );
    assert!(
        Id3v2Reader::try_new(source(&data, true), Default::default())
            .unwrap()
            .read_all()
            .is_err()
    );
    assert_eq!(
        Id3v2Reader::total_tag_len(data[..10].try_into().unwrap()).unwrap() as usize,
        data.len() - 5
    );
}
#[test]
fn full_mp3_with_compressed_optional_id3_keeps_the_same_pcm() {
    let sound = crate::mp3_fixture::stream(24);
    let expected = crate::candidate::decode(&sound, 1_000_000).unwrap();
    let mut data = compressed_id3v22(128 * 1024);
    data.extend(&sound);
    for seekable in [false, true] {
        let actual = crate::candidate::decode_source(&data, 1_000_000, seekable).unwrap();
        assert_eq!(actual.samples, expected.samples);
    }
}
#[test]
fn wave_info_is_retained_by_default_and_omitted_when_disabled() {
    let data = wave(b"kept\0");
    for seekable in [false, true] {
        assert!(
            metadata(&data, Default::default(), seekable).1 > 0,
            "WAV discarded parsed INFO"
        );
        assert_eq!(metadata(&data, disabled(), seekable), (0, 0, 0, 8000));
    }
}
#[test]
fn wave_large_optional_info_keeps_pcm_without_parsing_its_body() {
    let warm = wave(b"warm\0");
    metadata(&warm, Default::default(), true);
    let data = wave(&vec![b'x'; 2 * 1024 * 1024]);
    let input = source(&data, true);
    let before = migo_alloc_probe::thread_counts().bytes_allocated;
    let mut reader = symphonia::default::get_probe()
        .probe(&Hint::new(), input, FormatOptions::default(), disabled())
        .unwrap();
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before;
    assert!(reader.metadata().current().is_none());
    assert!(
        allocated < 128 * 1024,
        "WAV allocated unused INFO: {allocated}"
    );
    let expected = crate::candidate::decode(&wave(&[]), 1_000_000).unwrap();
    for seekable in [false, true] {
        assert_eq!(
            crate::candidate::decode_source(&data, 1_000_000, seekable)
                .unwrap()
                .samples,
            expected.samples
        );
    }
}
#[test]
fn mp4_nested_and_top_level_metadata_follow_the_same_policy() {
    for name in ["mp4-nested.m4a", "mp4-top.m4a"] {
        let data = fixture(name);
        assert!(metadata(&data, Default::default(), true).1 > 0);
        assert_eq!(metadata(&data, disabled(), true), (0, 0, 0, 48000));
        let expected = crate::candidate::decode(&fixture("mp4-clean.m4a"), 1_000_000).unwrap();
        assert_eq!(
            crate::candidate::decode(&data, 1_000_000).unwrap().samples,
            expected.samples
        );
    }
}
#[test]
fn mp4_skip_does_not_allocate_large_ilst_values() {
    let data = fixture("mp4-nested.m4a");
    metadata(&data, Default::default(), true);
    let input = source(&data, true);
    let before = migo_alloc_probe::thread_counts().bytes_allocated;
    let mut reader = symphonia::default::get_probe()
        .probe(&Hint::new(), input, FormatOptions::default(), disabled())
        .unwrap();
    let allocated = migo_alloc_probe::thread_counts().bytes_allocated - before;
    assert!(reader.metadata().current().is_none());
    assert!(
        allocated < 256 * 1024,
        "MP4 allocated unused ilst: {allocated}"
    );
}
#[test]
fn ogg_vorbis_zero_retention_preserves_setup_and_decoded_pcm() {
    let data = fixture("vorbis-large.ogg");
    for seekable in [false, true] {
        assert!(metadata(&data, Default::default(), seekable).1 > 0);
        assert_eq!(metadata(&data, disabled(), seekable), (0, 0, 0, 48000));
        let expected = crate::candidate::decode_retaining_metadata(&data, 1_000_000).unwrap();
        let actual = crate::candidate::decode_source(&data, 1_000_000, seekable).unwrap();
        assert!(
            actual.samples == expected.samples,
            "metadata policy changed Vorbis PCM: {} vs {} samples, first mismatch {:?}",
            actual.samples.len(),
            expected.samples.len(),
            actual
                .samples
                .iter()
                .zip(&expected.samples)
                .position(|(a, b)| a != b)
        );
    }
}
#[cfg(feature = "metadata-skip")]
#[test]
fn embedded_vorbis_skip_checks_lengths_without_value_or_base64_allocation() {
    use symphonia::core::meta::MetadataBuilder;
    use symphonia_metadata::embedded::vorbis::{
        VORBIS_COMMENT_METADATA_INFO, read_vorbis_comment_with_options,
    };
    let value = [
        b"METADATA_BLOCK_PICTURE=".as_slice(),
        &vec![b'A'; 2 * 1024 * 1024],
    ]
    .concat();
    let mut data = 0u32.to_le_bytes().to_vec();
    data.extend(1u32.to_le_bytes());
    data.extend((value.len() as u32).to_le_bytes());
    data.extend(value);
    data.extend(b"MEDIA");
    let mut reader = BufReader::new(&data);
    let mut builder = MetadataBuilder::new(VORBIS_COMMENT_METADATA_INFO);
    let mut side = Vec::new();
    let before = migo_alloc_probe::thread_counts().bytes_allocated;
    read_vorbis_comment_with_options(&mut reader, &mut builder, &mut side, disabled()).unwrap();
    assert_eq!(migo_alloc_probe::thread_counts().bytes_allocated, before);
    assert!(side.is_empty());
    assert!(builder.build().media.visuals.is_empty());
    assert_eq!(reader.read_boxed_slice_exact(5).unwrap().as_ref(), b"MEDIA");
    let mut truncated = 0u32.to_le_bytes().to_vec();
    truncated.extend(1u32.to_le_bytes());
    truncated.extend(u32::MAX.to_le_bytes());
    assert!(
        read_vorbis_comment_with_options(
            &mut BufReader::new(&truncated),
            &mut MetadataBuilder::new(VORBIS_COMMENT_METADATA_INFO),
            &mut Vec::new(),
            disabled()
        )
        .is_err()
    );
}
#[cfg(feature = "metadata-skip")]
#[test]
fn direct_wave_constructor_and_probe_metadata_precedence_are_explicit() {
    use symphonia::core::formats::FormatReader;
    let data = wave(b"kept\0");
    let mut direct = symphonia_format_riff::WavReader::try_new(
        source(&data, true),
        FormatOptions::default().metadata_options(disabled()),
    )
    .unwrap();
    assert!(direct.metadata().current().is_none());
    let mut reader = symphonia::default::get_probe()
        .probe(
            &Hint::new(),
            source(&data, true),
            FormatOptions::default().metadata_options(disabled()),
            MetadataOptions::default(),
        )
        .unwrap();
    assert!(
        reader.metadata().current().is_some(),
        "Probe's explicit metadata argument did not reach the format reader"
    );
}

#[test]
fn ape_default_rejects_unparsed_items_but_disabled_values_remain_opaque() {
    let original = ape(b"kept", true);
    let mut data = original[..original.len() - 32].to_vec();
    data.extend(b"junk");
    data.extend(&original[original.len() - 32..]);
    let size = u32::from_le_bytes(data[12..16].try_into().unwrap()) + 4;
    data[12..16].copy_from_slice(&size.to_le_bytes());
    let footer = data.len() - 32;
    data[footer + 12..footer + 16].copy_from_slice(&size.to_le_bytes());
    assert!(
        ApeReader::try_new(source(&data, true), Default::default())
            .unwrap()
            .read_all()
            .is_err()
    );
    assert!(
        ApeReader::try_new(source(&data, true), disabled())
            .unwrap()
            .read_all()
            .is_ok()
    );
}
