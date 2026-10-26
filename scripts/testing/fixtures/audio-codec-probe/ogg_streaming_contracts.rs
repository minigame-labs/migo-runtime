//! Real Vorbis packets, timing, cross-page assembly and bounded metadata.

use std::io::Cursor;
use symphonia_codec_vorbis::VorbisDecoder;
use symphonia_core::{
    codecs::audio::{AudioDecoder, AudioDecoderOptions},
    common::Limit,
    formats::{FormatOptions, FormatReader},
    io::MediaSourceStream,
    meta::MetadataOptions,
};
use symphonia_format_ogg::{
    OggReader,
    streaming::{PacketStep, VorbisPacketizer},
};
#[global_allocator]
static ALLOC: migo_alloc_probe::CountingAllocator = migo_alloc_probe::CountingAllocator::system();
fn data(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::PathBuf::from(std::env::var_os("W09_OGG_FIXTURES").unwrap()).join(name),
    )
    .unwrap()
}
fn whole(file: &[u8]) -> Vec<f32> {
    let mut options = FormatOptions::default();
    options.metadata_options = MetadataOptions::default().limit_total_bytes(Limit::Maximum(0));
    let mut format = OggReader::try_new(
        MediaSourceStream::new(Box::new(Cursor::new(file.to_vec())), Default::default()),
        options,
    )
    .unwrap();
    let mut codec = VorbisDecoder::try_new(
        format.tracks()[0]
            .codec_params
            .as_ref()
            .unwrap()
            .audio()
            .unwrap(),
        &AudioDecoderOptions::default().gapless(true),
    )
    .unwrap();
    let mut out = Vec::new();
    while let Some(packet) = format.next_packet().unwrap() {
        let block = codec.decode(&packet).unwrap();
        let start = out.len();
        out.resize(start + block.samples_interleaved(), 0.);
        block.copy_to_slice_interleaved(&mut out[start..]);
    }
    out
}
fn pushed(file: &[u8], fragment: usize) -> Vec<f32> {
    let mut parser = VorbisPacketizer::new(Default::default());
    let mut codec = None;
    let (mut offset, mut available) = (0, 0);
    let mut output = Vec::new();
    loop {
        match parser
            .next(&file[offset..available], available == file.len())
            .unwrap()
        {
            PacketStep::NeedMore => {
                assert!(available < file.len());
                available = (available + fragment).min(file.len()).min(offset + 65536);
                assert!(available > offset);
            }
            PacketStep::Discard(n) => {
                assert!(n > 0 && n <= 65307);
                offset += n;
            }
            PacketStep::Packet { consumed } => {
                let codec = codec.get_or_insert_with(|| {
                    VorbisDecoder::try_new(
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
                let packet = parser.packet(&file[offset..available]).unwrap();
                let block = codec.decode_ref(&packet).unwrap();
                let start = output.len();
                let channels = block.spec().channels().count();
                let frames = block.samples_interleaved() / channels;
                if frames != 0 {
                    assert_eq!(
                        packet.pts.get() + packet.trim_start.get() as i64,
                        (start / channels) as i64,
                        "packet PCM timeline drifted"
                    );
                    assert_eq!(
                        packet.dur.get(),
                        frames as u64,
                        "packet valid duration drifted"
                    );
                }
                output.resize(start + block.samples_interleaved(), 0.);
                block.copy_to_slice_interleaved(&mut output[start..]);
                offset += consumed;
            }
            PacketStep::End => {
                assert_eq!(offset, file.len());
                return output;
            }
        }
    }
}
#[test]
fn real_vorbis_partitions_equal_existing_pull_pcm() {
    let mut count = 0;
    for name in ["0.ogg", "1.ogg", "2.ogg", "3.ogg", "large.ogg", "cross.ogg"] {
        let file = data(name);
        let expected = whole(&file);
        assert!(!expected.is_empty());
        // Pull and push share timing code, so also check the independent PCM
        // source lengths. Comparing only the two readers can miss a shared bug.
        let (source_frames, channels) = match name {
            "0.ogg" => (17, 1),
            "1.ogg" | "large.ogg" => (9737, 2),
            "2.ogg" => (9737, 1),
            "3.ogg" | "cross.ogg" => (240137, 6),
            _ => unreachable!(),
        };
        assert_eq!(
            expected.len(),
            source_frames * channels,
            "{name}: source length drifted"
        );
        for n in [1, 3, 137, 8192, 65536] {
            assert_eq!(pushed(&file, n), expected, "{name}/{n}");
            count += 1;
        }
    }
    assert_eq!(whole(&data("3.ogg")), pushed(&data("cross.ogg"), 137));
    assert_eq!(whole(&data("clean.ogg")), pushed(&data("large.ogg"), 137));
    println!(
        "OGG_REAL {count} pull/push comparisons plus independent repagination/metadata equivalence"
    );
}
fn packetize_result(
    file: &[u8],
    options: symphonia_format_ogg::streaming::PacketizerOptions,
) -> Result<(), String> {
    let mut parser = VorbisPacketizer::new(options);
    let mut offset = 0;
    loop {
        match parser
            .next(&file[offset..], true)
            .map_err(|e| e.to_string())?
        {
            PacketStep::Discard(n) | PacketStep::Packet { consumed: n } => offset += n,
            PacketStep::End => return Ok(()),
            PacketStep::NeedMore => panic!("EOF requested more bytes"),
        }
    }
}
#[test]
fn complete_packets_obey_setup_and_audio_working_set_quotas() {
    let file = data("1.ogg");
    assert!(
        packetize_result(
            &file,
            symphonia_format_ogg::streaming::PacketizerOptions {
                max_setup_bytes: 64,
                ..Default::default()
            }
        )
        .is_err(),
        "complete setup escaped limit"
    );
    assert!(
        packetize_result(
            &file,
            symphonia_format_ogg::streaming::PacketizerOptions {
                max_packet_bytes: 1,
                ..Default::default()
            }
        )
        .is_err(),
        "complete audio escaped limit"
    );
}
fn rewrite_page(file: &[u8], index: usize, change: impl FnOnce(&mut [u8])) -> Vec<u8> {
    let mut output = file.to_vec();
    let mut start = 0;
    for _ in 0..index {
        let n = output[start + 26] as usize;
        start += 27
            + n
            + output[start + 27..start + 27 + n]
                .iter()
                .map(|n| *n as usize)
                .sum::<usize>();
    }
    let n = output[start + 26] as usize;
    let end = start
        + 27
        + n
        + output[start + 27..start + 27 + n]
            .iter()
            .map(|n| *n as usize)
            .sum::<usize>();
    change(&mut output[start..end]);
    output[start + 22..start + 26].fill(0);
    let mut crc = 0_u32;
    for &byte in &output[start..end] {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x80000000 != 0 {
                (crc << 1) ^ 0x04c11db7
            } else {
                crc << 1
            };
        }
    }
    output[start + 22..start + 26].copy_from_slice(&crc.to_le_bytes());
    output
}
fn page_count(file: &[u8]) -> usize {
    let (mut offset, mut count) = (0, 0);
    while offset < file.len() {
        let n = file[offset + 26] as usize;
        offset += 27
            + n
            + file[offset + 27..offset + 27 + n]
                .iter()
                .map(|n| *n as usize)
                .sum::<usize>();
        count += 1;
    }
    count
}
#[test]
fn logical_eos_sequence_serial_continuation_and_granules_are_strict() {
    let good = data("3.ogg");
    let cross = data("cross.ogg");
    let count = page_count(&good);
    let mut invalid = vec![
        good[..1].to_vec(),
        good[..26].to_vec(),
        good[..good.len() - 1].to_vec(),
    ];
    invalid.push(rewrite_page(&good, count - 1, |page| page[5] &= !4));
    invalid.push(rewrite_page(&good, 2, |page| page[14] ^= 1));
    invalid.push(rewrite_page(&good, 2, |page| {
        let seq = u32::from_le_bytes(page[18..22].try_into().unwrap());
        page[18..22].copy_from_slice(&(seq + 1).to_le_bytes());
    }));
    invalid.push(rewrite_page(&good, count - 1, |page| page[6..14].fill(0)));
    let mut combined = good.clone();
    combined.extend_from_slice(&good);
    invalid.push(combined);
    let mut offset = 0;
    let mut index = 0;
    while offset < cross.len() {
        let n = cross[offset + 26] as usize;
        if cross[offset + 5] & 1 != 0 {
            break;
        }
        offset += 27
            + n
            + cross[offset + 27..offset + 27 + n]
                .iter()
                .map(|n| *n as usize)
                .sum::<usize>();
        index += 1;
    }
    invalid.push(rewrite_page(&cross, index, |page| page[5] &= !1));
    for file in invalid {
        let mut parser = VorbisPacketizer::new(Default::default());
        let mut offset = 0;
        loop {
            match parser.next(&file[offset..], true) {
                Ok(PacketStep::Discard(n)) | Ok(PacketStep::Packet { consumed: n }) => offset += n,
                Err(_) => {
                    assert!(parser.next(&good, true).is_err(), "failure was not sticky");
                    break;
                }
                _ => panic!("malformed logical stream became successful EOF"),
            }
        }
    }
    assert!(
        packetize_result(
            &good,
            symphonia_format_ogg::streaming::PacketizerOptions {
                max_metadata_bytes: 0,
                ..Default::default()
            }
        )
        .is_err()
    );
}
#[test]
fn large_discarded_comment_has_the_same_rust_allocation_as_clean_audio() {
    let clean = data("clean.ogg");
    let large = data("large.ogg");
    packetize_result(&clean, Default::default()).unwrap();
    let measure = |file: &[u8]| {
        let before = migo_alloc_probe::thread_counts();
        packetize_result(file, Default::default()).unwrap();
        let after = migo_alloc_probe::thread_counts();
        (
            after.allocation_events() - before.allocation_events(),
            after.bytes_allocated - before.bytes_allocated,
        )
    };
    let a = measure(&clean);
    let b = measure(&large);
    assert_eq!(a, b, "large metadata caused retained/repeated allocations");
    println!("OGG_METADATA_ALLOCS clean={a:?} large={b:?}");
}
#[test]
fn real_vorbis_packetize_decode_convert_has_no_steady_rust_allocation() {
    let file = data("3.ogg");
    let mut parser = VorbisPacketizer::new(Default::default());
    let mut codec = None;
    let mut out = vec![0.; 32768];
    let mut offset = 0;
    let mut packets = 0;
    let mut step = || {
        loop {
            match parser.next(&file[offset..], false).unwrap() {
                PacketStep::Discard(n) => offset += n,
                PacketStep::Packet { consumed } => {
                    let codec = codec.get_or_insert_with(|| {
                        VorbisDecoder::try_new(
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
                    let packet = parser.packet(&file[offset..]).unwrap();
                    let block = codec.decode_ref(&packet).unwrap();
                    let n = block.samples_interleaved();
                    block.copy_to_slice_interleaved(&mut out[..n]);
                    offset += consumed;
                    packets += 1;
                    break;
                }
                _ => panic!("fixture ended before measured packets"),
            }
        }
    };
    for _ in 0..32 {
        step();
    }
    let before = migo_alloc_probe::thread_counts().allocation_events();
    for _ in 0..96 {
        step();
    }
    let after = migo_alloc_probe::thread_counts().allocation_events();
    assert_eq!(after - before, 0);
    assert_eq!(packets, 128);
}

#[test]
fn bitstream_eof_is_precise_and_has_no_rust_allocation() {
    use symphonia_core::io::{BitReaderLtr, BitReaderRtl, ReadBitsLtr, ReadBitsRtl};
    // Bit-level EOF remains an error to callers; codecs decide which EOF is
    // legal. Wide reads across a short packet must not allocate an error box.
    let before = migo_alloc_probe::thread_counts().allocation_events();
    for _ in 0..256 {
        for width in 1..=64 {
            let mut ltr = BitReaderLtr::new(&[]);
            let mut rtl = BitReaderRtl::new(&[]);
            assert_eq!(
                ltr.read_bits_leq64(width).unwrap_err().kind(),
                std::io::ErrorKind::UnexpectedEof
            );
            assert_eq!(
                rtl.read_bits_leq64(width).unwrap_err().kind(),
                std::io::ErrorKind::UnexpectedEof
            );
        }
        assert_eq!(
            BitReaderRtl::new(&[0x0e])
                .read_bits_leq32(9)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }
    let after = migo_alloc_probe::thread_counts().allocation_events();
    assert_eq!(after - before, 0);
    for width in 1..=64 {
        let data = [0xa5_u8; 8];
        assert_eq!(
            BitReaderLtr::new(&data).read_bits_leq64(width).unwrap(),
            u64::from_be_bytes(data) >> (64 - width)
        );
        let mask = u64::MAX >> (64 - width);
        assert_eq!(
            BitReaderRtl::new(&data).read_bits_leq64(width).unwrap(),
            u64::from_le_bytes(data) & mask
        );
    }
}
