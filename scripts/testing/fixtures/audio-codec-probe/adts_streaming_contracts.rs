//! Real AAC-LC decode, strict EOF, borrowed packet and allocation checks.

use std::io::Cursor;
use symphonia_codec_aac::{
    AacDecoder, AdtsReader,
    streaming::{AdtsPacketizer, PacketStep},
};
use symphonia_core::{
    codecs::audio::{AudioDecoder, AudioDecoderOptions},
    formats::{FormatOptions, FormatReader},
    io::MediaSourceStream,
};
#[global_allocator]
static ALLOC: migo_alloc_probe::CountingAllocator = migo_alloc_probe::CountingAllocator::system();
fn fixture(i: u32) -> Vec<u8> {
    std::fs::read(
        std::path::PathBuf::from(std::env::var_os("W09_ADTS_FIXTURES").unwrap())
            .join(format!("{i}.aac")),
    )
    .unwrap()
}
fn whole(file: &[u8]) -> Vec<f32> {
    let mut format = AdtsReader::try_new(
        MediaSourceStream::new(Box::new(Cursor::new(file.to_vec())), Default::default()),
        FormatOptions::default(),
    )
    .unwrap();
    assert!(
        format.tracks()[0].num_frames.is_none(),
        "bitrate estimate became an exact frame count"
    );
    let mut codec = AacDecoder::try_new(
        format.tracks()[0]
            .codec_params
            .as_ref()
            .unwrap()
            .audio()
            .unwrap(),
        &AudioDecoderOptions::default(),
    )
    .unwrap();
    let mut output = Vec::new();
    while let Some(packet) = format.next_packet().unwrap() {
        let block = codec.decode(&packet).unwrap();
        let start = output.len();
        output.resize(start + block.samples_interleaved(), 0.);
        block.copy_to_slice_interleaved(&mut output[start..]);
    }
    output
}
fn pushed(file: &[u8], fragment: usize) -> Vec<f32> {
    let mut stream = AdtsPacketizer::new(Default::default());
    let mut codec = None;
    let (mut offset, mut available) = (0, 0);
    let mut output = Vec::new();
    loop {
        match stream
            .next(&file[offset..available], available == file.len())
            .unwrap()
        {
            PacketStep::NeedMore => {
                assert!(available < file.len());
                available = (available + fragment).min(file.len());
            }
            PacketStep::Discard(n) => {
                assert!(n > 0 && n <= 4096);
                offset += n;
            }
            PacketStep::Packet { consumed, packet } => {
                let codec = codec.get_or_insert_with(|| {
                    AacDecoder::try_new(
                        stream
                            .track()
                            .unwrap()
                            .codec_params
                            .as_ref()
                            .unwrap()
                            .audio()
                            .unwrap(),
                        &AudioDecoderOptions::default(),
                    )
                    .unwrap()
                });
                let block = codec.decode_ref(&packet).unwrap();
                let start = output.len();
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
fn real_aac_lc_partitions_equal_pull_pcm() {
    let mut count = 0;
    for i in 0..4 {
        let file = fixture(i);
        let expected = whole(&file);
        assert!(!expected.is_empty());
        for n in [1, 3, 137, 8192, 65536] {
            assert_eq!(pushed(&file, n), expected, "fixture{i}/{n}");
            count += 1;
        }
    }
    println!("ADTS_REAL_PCM {count} exact comparisons");
}
#[test]
fn pull_rejects_partial_headers_after_valid_audio() {
    for tail in [
        vec![0xff],
        vec![0xff, 0xf1, 0x4c],
        vec![0xff, 0xf0, 0x4c, 0x80, 0, 0, 0],
    ] {
        let mut file = fixture(1);
        file.extend_from_slice(&tail);
        let mut format = AdtsReader::try_new(
            MediaSourceStream::new(Box::new(Cursor::new(file)), Default::default()),
            FormatOptions::default(),
        )
        .unwrap();
        loop {
            match format.next_packet() {
                Ok(Some(_)) => {}
                Err(_) => break,
                Ok(None) => panic!("partial ADTS header became EOF"),
            }
        }
    }
}
#[test]
fn actual_borrowed_aac_decode_and_convert_has_no_steady_rust_allocation() {
    for i in [0, 1, 2, 3] {
        let file = fixture(i);
        let mut stream = AdtsPacketizer::new(Default::default());
        let PacketStep::Packet { packet, .. } = stream.next(&file, false).unwrap() else {
            panic!()
        };
        let mut codec = AacDecoder::try_new(
            stream
                .track()
                .unwrap()
                .codec_params
                .as_ref()
                .unwrap()
                .audio()
                .unwrap(),
            &AudioDecoderOptions::default(),
        )
        .unwrap();
        let mut output = vec![0.; 2048];
        for _ in 0..32 {
            let block = codec.decode_ref(&packet).unwrap();
            let n = block.samples_interleaved();
            block.copy_to_slice_interleaved(&mut output[..n]);
        }
        let before = migo_alloc_probe::thread_counts().allocation_events();
        for _ in 0..256 {
            let PacketStep::Packet { packet, .. } = stream.next(&file, false).unwrap() else {
                panic!()
            };
            let block = codec.decode_ref(&packet).unwrap();
            let n = block.samples_interleaved();
            block.copy_to_slice_interleaved(&mut output[..n]);
        }
        let after = migo_alloc_probe::thread_counts().allocation_events();
        assert_eq!(after - before, 0, "fixture{i}");
    }
}
