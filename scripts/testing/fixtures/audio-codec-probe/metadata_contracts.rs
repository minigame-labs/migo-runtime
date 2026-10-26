//! Exercises published reader APIs, decoded metadata and actual allocations.
use std::io::Cursor;
use symphonia::core::{
    common::Limit,
    io::{MediaSourceStream, MediaSourceStreamOptions, ReadBytes},
    meta::{MetadataOptions, MetadataReader, RawValue},
};
use symphonia_metadata::id3v2::Id3v2Reader;

#[global_allocator]
static COUNTING: migo_alloc_probe::CountingAllocator =
    migo_alloc_probe::CountingAllocator::system();

fn syncsafe(size: usize) -> [u8; 4] {
    assert!(size < 1 << 28);
    [
        (size >> 21) as u8 & 127,
        (size >> 14) as u8 & 127,
        (size >> 7) as u8 & 127,
        size as u8 & 127,
    ]
}
fn frame(version: u8, id: &[u8], data: &[u8], flags: u16) -> Vec<u8> {
    let mut out = id.to_vec();
    if version == 2 {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes()[1..]);
    } else {
        out.extend_from_slice(&if version == 4 {
            syncsafe(data.len())
        } else {
            (data.len() as u32).to_be_bytes()
        });
        out.extend_from_slice(&flags.to_be_bytes());
    }
    out.extend_from_slice(data);
    out
}
fn text(version: u8, value: &[u8], encoding: u8) -> Vec<u8> {
    let mut data = vec![encoding];
    data.extend_from_slice(value);
    frame(
        version,
        if version == 2 { b"TT2" } else { b"TIT2" },
        &data,
        0,
    )
}
fn tag(version: u8, body: &[u8], footer: bool) -> Vec<u8> {
    let mut out = vec![b'I', b'D', b'3', version, 0, if footer { 16 } else { 0 }];
    out.extend_from_slice(&syncsafe(body.len()));
    out.extend_from_slice(body);
    if footer {
        let mut bytes = out[..10].to_vec();
        bytes[..3].copy_from_slice(b"3DI");
        out.extend_from_slice(&bytes);
    }
    out
}
fn unsynchronised_tag(version: u8, body: &[u8]) -> Vec<u8> {
    assert!(version == 2 || version == 3);
    let mut encoded = Vec::new();
    for &byte in body {
        encoded.push(byte);
        if byte == 0xff {
            encoded.push(0);
        }
    }
    let mut result = tag(version, &encoded, false);
    result[5] = 0x80;
    result
}
fn options(bytes: usize) -> MetadataOptions {
    MetadataOptions::default()
        .limit_tag_bytes(Limit::Maximum(bytes))
        .limit_visual_bytes(Limit::Maximum(0))
}
fn reader(data: &[u8], opts: MetadataOptions) -> Id3v2Reader<'_> {
    Id3v2Reader::try_new(
        MediaSourceStream::new(
            Box::new(Cursor::new(data)),
            MediaSourceStreamOptions::default(),
        ),
        opts,
    )
    .unwrap()
}
fn strings(data: &[u8], opts: MetadataOptions) -> Vec<String> {
    reader(data, opts)
        .read_all()
        .unwrap()
        .revision
        .media
        .tags
        .iter()
        .filter_map(|tag| {
            if let RawValue::String(value) = &tag.raw.value {
                Some(value.as_str().to_owned())
            } else {
                None
            }
        })
        .collect()
}
fn measured_read(
    data: &[u8],
    opts: MetadataOptions,
) -> (symphonia::core::meta::MetadataBuffer, u64) {
    // Warm the library's frame lookup, and prove the allocator is installed,
    // before measuring. Source bytes and the stream ring are outside the window.
    let warm = tag(4, &text(4, b"warm", 3), false);
    reader(&warm, options(1024)).read_all().unwrap();
    let before = migo_alloc_probe::thread_counts();
    std::hint::black_box(vec![0_u8; 64]);
    assert!(migo_alloc_probe::thread_counts().allocation_events() > before.allocation_events());
    let mut input = reader(data, opts);
    let before = migo_alloc_probe::thread_counts().bytes_allocated;
    let result = input.read_all().unwrap();
    (
        result,
        migo_alloc_probe::thread_counts().bytes_allocated - before,
    )
}

#[test]
fn oversized_tags_are_skipped_without_losing_following_tags() {
    for version in [2, 3, 4] {
        let mut body = text(version, &vec![b'x'; 4096], 0);
        body.extend(text(version, b"kept", 0));
        assert_eq!(
            strings(&tag(version, &body, false), options(1024)),
            ["kept"]
        );
    }
}
#[test]
fn decoded_text_expansion_is_charged_to_the_tag_limit() {
    for version in [2, 3, 4] {
        let mut body = text(version, &vec![0xe9; 400], 0);
        body.extend(text(version, b"kept", 0));
        assert_eq!(strings(&tag(version, &body, false), options(512)), ["kept"]);
    }
}
#[test]
fn disabled_pictures_are_skipped_before_their_payload_is_allocated() {
    for version in [2, 3, 4] {
        let mut picture = if version == 2 {
            b"\0PNG\x03cover\0".to_vec()
        } else {
            b"\0image/png\0\x03cover\0".to_vec()
        };
        picture.resize(2 * 1024 * 1024, 0x55);
        let mut body = frame(
            version,
            if version == 2 { b"PIC" } else { b"APIC" },
            &picture,
            0,
        );
        body.extend(text(version, b"kept", 0));
        let data = tag(version, &body, false);
        let (result, allocated) = measured_read(
            &data,
            options(1024).limit_frame_bytes(Limit::Maximum(4 * 1024 * 1024)),
        );
        assert!(result.revision.media.visuals.is_empty());
        assert!(
            allocated < 64 * 1024,
            "picture bytes allocated despite visual=0: {allocated}"
        );
        assert_eq!(result.revision.media.tags.len(), 1);
    }
}
#[test]
fn oversized_frame_skipping_has_a_bounded_temporary_allocation() {
    for version in [2, 3, 4] {
        let mut body = text(version, &vec![b'x'; 4 * 1024 * 1024], 0);
        body.extend(text(version, b"kept", 0));
        let data = tag(version, &body, false);
        let (result, allocated) = measured_read(&data, options(1024));
        assert!(
            allocated < 1024 * 1024,
            "oversized frame was allocated before skipping: {allocated}"
        );
        assert_eq!(result.revision.media.tags.len(), 1);
    }
}
#[test]
fn footer_is_consumed_before_the_next_media_byte() {
    let mut data = tag(4, &text(4, b"kept", 3), true);
    data.extend_from_slice(b"MEDIA");
    let mut input = reader(&data, options(1024));
    input.read_all().unwrap();
    let mut stream = Box::new(input).into_inner();
    let mut marker = [0; 5];
    stream.read_buf_exact(&mut marker).unwrap();
    assert_eq!(&marker, b"MEDIA");
}
#[test]
fn compressed_frames_remain_skipped_without_an_inflater() {
    for version in [3, 4] {
        let mut compressed = if version == 3 {
            u32::MAX.to_be_bytes().to_vec()
        } else {
            syncsafe((1 << 28) - 1).to_vec()
        };
        compressed.extend_from_slice(b"unsupported compressed bytes");
        let mut body = frame(
            version,
            b"TIT2",
            &compressed,
            if version == 3 { 0x80 } else { 0x09 },
        );
        body.extend(text(version, b"kept", 0));
        let data = tag(version, &body, false);
        let (result, allocated) = measured_read(&data, options(1024));
        assert_eq!(result.revision.media.tags.len(), 1);
        assert!(allocated < 64 * 1024);
    }
}

#[test]
fn no_retained_metadata_skips_the_whole_optional_body() {
    let mut body = text(4, &vec![b'x'; 2 * 1024 * 1024], 3);
    body.extend(text(4, b"not retained", 3));
    let data = tag(4, &body, true);
    let (result, allocated) =
        measured_read(&data, options(1024).limit_total_bytes(Limit::Maximum(0)));
    assert!(result.revision.media.tags.is_empty());
    assert!(result.revision.media.visuals.is_empty());
    assert!(result.side_data.is_empty());
    assert!(
        allocated < 4096,
        "zero retention decoded optional metadata: {allocated}"
    );
}

#[test]
fn cumulative_budget_covers_metadata_vector_spare_capacity() {
    let mut body = Vec::new();
    for _ in 0..1000 {
        body.extend(text(4, b"a retained tag", 3));
    }
    let result = reader(
        &tag(4, &body, false),
        options(1024).limit_total_bytes(Limit::Maximum(2048)),
    )
    .read_all()
    .unwrap();
    let tags = &result.revision.media.tags;
    assert!(!tags.is_empty() && tags.len() < 1000);
    let bytes = tags.iter().fold(
        tags.capacity() * std::mem::size_of::<symphonia::core::meta::Tag>(),
        |size, value| {
            size + value.storage_size_upper_bound()
                - std::mem::size_of::<symphonia::core::meta::Tag>()
        },
    );
    assert!(
        bytes <= 2048,
        "retained tags/spare capacity exceeded quota: {bytes}"
    );
}

fn chapter(inner: &[u8]) -> Vec<u8> {
    let mut body = b"chapter\0".to_vec();
    body.extend_from_slice(&0_u32.to_be_bytes());
    for _ in 0..3 {
        body.extend_from_slice(&u32::MAX.to_be_bytes());
    }
    body.extend_from_slice(inner);
    frame(4, b"CHAP", &body, 0)
}

#[test]
fn nested_frame_depth_bounds_temporary_copies() {
    let mut nested = text(4, b"deep", 3);
    for _ in 0..256 {
        nested = chapter(&nested);
    }
    let data = tag(4, &nested, false);
    let (_, allocated) = measured_read(&data, options(1024).limit_nesting_depth(Limit::Maximum(2)));
    assert!(
        allocated < 128 * 1024,
        "nested frame copies escaped the depth budget: {allocated}"
    );
}

#[test]
fn toc_graph_depth_is_bounded_independently_of_frame_nesting() {
    let mut body = Vec::new();
    for index in 0..64 {
        let mut data = format!("t{index}\0").into_bytes();
        data.push(if index == 0 { 2 } else { 0 });
        data.push(1);
        data.extend_from_slice(format!("t{}\0", index + 1).as_bytes());
        body.extend(frame(4, b"CTOC", &data, 0));
    }
    let result = reader(
        &tag(4, &body, false),
        options(1024).limit_nesting_depth(Limit::Maximum(2)),
    )
    .read_all()
    .unwrap();
    fn depth(group: &symphonia::core::meta::ChapterGroup) -> usize {
        group
            .items
            .iter()
            .filter_map(|item| match item {
                symphonia::core::meta::ChapterGroupItem::Group(child) => Some(1 + depth(child)),
                _ => None,
            })
            .max()
            .unwrap_or(0)
    }
    assert!(!result.side_data.is_empty());
    for data in result.side_data {
        let symphonia::core::meta::MetadataSideData::Chapters(group) = data else {
            panic!("unexpected side data")
        };
        assert!(
            depth(&group) <= 2,
            "TOC recursion exceeded the configured group depth"
        );
    }
}

#[test]
fn toc_output_slot_capacity_is_charged_before_building() {
    let mut data = b"root\0\x02\xff".to_vec();
    for index in 0..255 {
        data.extend_from_slice(format!("missing{index}\0").as_bytes());
    }
    let input = tag(4, &frame(4, b"CTOC", &data, 0), false);
    let result = reader(
        &input,
        options(1024).limit_total_bytes(Limit::Maximum(12 * 1024)),
    )
    .read_all()
    .unwrap();
    assert!(
        result.side_data.is_empty(),
        "TOC reserved large output slots despite an insufficient revision budget"
    );
}

#[test]
fn per_tag_limit_is_not_a_limit_on_all_encoded_id3_bytes() {
    let mut body = Vec::new();
    for _ in 0..40 {
        body.extend(text(4, b"kept", 3));
    }
    let values = strings(
        &tag(4, &body, false),
        options(512).limit_total_bytes(Limit::None),
    );
    assert_eq!(values.len(), 40);
}

#[test]
fn explicit_unlimited_values_and_an_independent_frame_limit_work() {
    let body = text(4, &vec![b'x'; 4096], 3);
    let data = tag(4, &body, false);
    let unlimited = MetadataOptions::default()
        .limit_tag_bytes(Limit::None)
        .limit_visual_bytes(Limit::None)
        .limit_frame_bytes(Limit::None)
        .limit_total_bytes(Limit::None)
        .limit_nesting_depth(Limit::None);
    assert_eq!(strings(&data, unlimited)[0].len(), 4096);
    assert!(strings(&data, unlimited.limit_frame_bytes(Limit::Maximum(1024))).is_empty());
}

#[test]
#[cfg(feature = "metadata-budgets")]
fn full_audio_decode_skips_large_optional_metadata_without_losing_pcm() {
    let sound = crate::mp3_fixture::stream(24);
    let expected =
        crate::candidate::decode(&sound, crate::limits::MAX_AUDIO_PCM_SAMPLES as usize).unwrap();
    for seekable in [false, true] {
        let body = text(4, &vec![b'x'; 4 * 1024 * 1024], 3);
        let mut input = tag(4, &body, true);
        input.extend_from_slice(&sound);
        let actual = crate::candidate::decode_source(
            &input,
            crate::limits::MAX_AUDIO_PCM_SAMPLES as usize,
            seekable,
        )
        .unwrap();
        assert_eq!(
            (actual.sample_rate, actual.channels),
            (expected.sample_rate, expected.channels)
        );
        assert_eq!(actual.samples, expected.samples);
    }
}

#[test]
#[cfg(feature = "metadata-budgets")]
fn no_retention_does_not_accumulate_empty_external_revisions() {
    use symphonia::core::formats::{FormatOptions, probe::Hint};
    let mut source = Vec::new();
    for _ in 0..4096 {
        source.extend_from_slice(&tag(4, &[], false));
    }
    source.extend_from_slice(&crate::mp3_fixture::stream(24));
    let stream = MediaSourceStream::new(
        Box::new(Cursor::new(source)),
        MediaSourceStreamOptions::default(),
    );
    let mut format = symphonia::default::get_probe()
        .probe(
            &Hint::new(),
            stream,
            FormatOptions::default(),
            options(1024).limit_total_bytes(Limit::Maximum(0)),
        )
        .unwrap();
    assert!(
        format.metadata().current().is_none(),
        "empty external revisions accumulated despite zero retention"
    );
}

#[test]
fn impossible_frame_extent_does_not_allocate_the_declared_size() {
    for version in [2, 3, 4] {
        for size in [
            128_u32 * 1024,
            if version == 2 {
                (1 << 24) - 1
            } else if version == 3 {
                u32::MAX
            } else {
                (1 << 28) - 1
            },
        ] {
            let mut encoded = frame(
                version,
                if version == 2 { b"TT2" } else { b"TIT2" },
                b"\0bad",
                0,
            );
            if version == 2 {
                encoded[3..6].copy_from_slice(&size.to_be_bytes()[1..]);
            } else if version == 3 {
                encoded[4..8].copy_from_slice(&size.to_be_bytes());
            } else {
                encoded[4..8].copy_from_slice(&syncsafe(size as usize));
            }
            let mut cases = vec![tag(version, &encoded, false)];
            if version < 4 {
                cases.push(unsynchronised_tag(version, &encoded));
            }
            for data in cases {
                let (result, allocated) =
                    measured_read(&data, options(1024).limit_frame_bytes(Limit::None));
                assert!(result.revision.media.tags.is_empty());
                assert!(
                    allocated < 64 * 1024,
                    "impossible size was allocated: {allocated}"
                );
            }
        }
    }
}

#[test]
#[ignore = "run in resource-bounded subprocesses with explicit corpus input"]
fn bounded_metadata_fuzz_case() {
    let path = std::env::var_os("W09_METADATA_FUZZ_INPUT").expect("requires explicit corpus input");
    let data = std::fs::read(path).unwrap();
    let opts = options(1024)
        .limit_total_bytes(Limit::Maximum(16 * 1024))
        .limit_frame_bytes(Limit::Maximum(64 * 1024))
        .limit_nesting_depth(Limit::Maximum(2));
    match reader(&data, opts).read_all() {
        Ok(result) => {
            assert!(result.revision.media.visuals.is_empty());
            assert!(
                result
                    .revision
                    .media
                    .tags
                    .iter()
                    .all(|value| value.storage_size_upper_bound() <= 1024)
            );
            println!("W09_METADATA_FUZZ accepted");
        }
        Err(_) => println!("W09_METADATA_FUZZ rejected"),
    }
}

#[test]
fn skipped_unsynchronised_frames_preserve_the_next_tag() {
    for version in [2, 3] {
        let mut body = text(version, &vec![0xff; 4096], 0);
        body.extend(text(version, b"kept", 0));
        let options = options(1024).limit_frame_bytes(Limit::Maximum(1024));
        assert_eq!(
            strings(&unsynchronised_tag(version, &body), options),
            ["kept"]
        );
    }
}
