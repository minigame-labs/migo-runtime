use std::io::Cursor;
use symphonia_codec_pcm::PcmDecoder;
use symphonia_core::{
    codecs::audio::{AudioDecoder, AudioDecoderOptions},
    formats::{FormatOptions, FormatReader},
    io::MediaSourceStream,
};
use symphonia_format_riff::{
    WavReader,
    streaming::{PacketStep, PacketizerOptions, WavPacketizer},
};

fn chunk(file: &mut Vec<u8>, tag: &[u8; 4], data: &[u8]) {
    file.extend_from_slice(tag);
    file.extend_from_slice(&(data.len() as u32).to_le_bytes());
    file.extend_from_slice(data);
    if data.len() % 2 != 0 {
        file.push(0);
    }
}
fn fixture(bits: u16, floating: bool, channels: u16, frames: usize) -> (Vec<u8>, Vec<f32>) {
    let mut raw = Vec::new();
    let mut oracle = Vec::new();
    for index in 0..frames * channels as usize {
        if floating {
            let value = [-1.0_f32, -0.75, -0.125, 0.0, 0.25, 0.625, 1.0][index % 7];
            raw.extend_from_slice(&value.to_le_bytes());
            oracle.push(value);
        } else if bits == 8 {
            let value = [0_u8, 1, 64, 127, 128, 129, 192, 254, 255][index % 9];
            raw.push(value);
            oracle.push((value as f32 - 128.0) / 128.0);
        } else {
            let magnitude = 1_i64 << (bits - 1);
            let value = [
                -magnitude,
                -magnitude + 1,
                -magnitude / 2,
                -1,
                0,
                1,
                magnitude / 2,
                magnitude - 1,
            ][index % 8];
            raw.extend_from_slice(&value.to_le_bytes()[..bits as usize / 8]);
            oracle.push(value as f32 / magnitude as f32);
        }
    }
    let mut file = b"RIFF\0\0\0\0WAVE".to_vec();
    let mut format = Vec::new();
    format.extend_from_slice(&(if floating { 3_u16 } else { 1_u16 }).to_le_bytes());
    format.extend_from_slice(&channels.to_le_bytes());
    format.extend_from_slice(&44100_u32.to_le_bytes());
    format.extend_from_slice(&(44100_u32 * channels as u32 * bits as u32 / 8).to_le_bytes());
    format.extend_from_slice(&(channels * bits / 8).to_le_bytes());
    format.extend_from_slice(&bits.to_le_bytes());
    chunk(&mut file, b"JUNK", &[7, 8, 9]);
    chunk(&mut file, b"fmt ", &format);
    if floating {
        chunk(&mut file, b"fact", &(frames as u32).to_le_bytes());
    }
    chunk(&mut file, b"data", &raw);
    let size = file.len() as u32 - 8;
    file[4..8].copy_from_slice(&size.to_le_bytes());
    (file, oracle)
}
fn whole(file: &[u8]) -> Vec<f32> {
    let source = MediaSourceStream::new(Box::new(Cursor::new(file.to_vec())), Default::default());
    let mut format = WavReader::try_new(source, FormatOptions::default()).unwrap();
    let params = format.tracks()[0]
        .codec_params
        .as_ref()
        .unwrap()
        .audio()
        .unwrap();
    let mut decoder = PcmDecoder::try_new(params, &AudioDecoderOptions::default()).unwrap();
    let mut output = Vec::new();
    while let Some(packet) = format.next_packet().unwrap() {
        let block = decoder.decode(&packet).unwrap();
        let start = output.len();
        output.resize(start + block.samples_interleaved(), 0.0);
        block.copy_to_slice_interleaved(&mut output[start..]);
    }
    output
}
fn pushed(file: &[u8], fragment: usize) -> Vec<f32> {
    let mut packetizer = WavPacketizer::new(PacketizerOptions::default());
    let (mut offset, mut available) = (0, 0);
    let mut decoder = None;
    let mut output = Vec::new();
    for _ in 0..file.len() * 3 + 1024 {
        match packetizer
            .next(&file[offset..available], available == file.len())
            .unwrap()
        {
            PacketStep::NeedMore => {
                assert!(available < file.len());
                available = (available + fragment).min(file.len());
            }
            PacketStep::Discard(n) => {
                assert!(n > 0);
                offset += n;
            }
            PacketStep::Packet { consumed, packet } => {
                let codec = decoder.get_or_insert_with(|| {
                    PcmDecoder::try_new(
                        packetizer
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
                output.resize(start + block.samples_interleaved(), 0.0);
                block.copy_to_slice_interleaved(&mut output[start..]);
                offset += consumed;
            }
            PacketStep::End => return output,
        }
    }
    panic!("push decode made no bounded progress");
}
#[test]
fn pcm_streams_match_numeric_oracle_and_existing_pull_decoder() {
    let mut count = 0;
    for channels in [1, 2, 6, 26] {
        for (bits, floating) in [
            (8, false),
            (16, false),
            (24, false),
            (32, false),
            (32, true),
        ] {
            for frames in [1, 17, 2317] {
                let (file, oracle) = fixture(bits, floating, channels, frames);
                let pulled = whole(&file);
                assert_eq!(
                    pulled, oracle,
                    "pull/numeric {channels}/{bits}/{floating}/{frames}"
                );
                for fragment in [1, 3, 137, 8192, 65536] {
                    let actual = pushed(&file, fragment);
                    assert_eq!(
                        actual, oracle,
                        "push/numeric {channels}/{bits}/{floating}/{frames}/{fragment}"
                    );
                    assert_eq!(actual, pulled);
                    count += 1;
                }
            }
        }
    }
    println!("W09_WAVE_PCM {count} numeric/pull/push comparisons");
    assert_eq!(count, 300);
}

#[test]
fn pcm_short_packets_and_partial_frames_obey_the_coded_length() {
    use symphonia_core::{
        packet::PacketRef,
        units::{Duration, Timestamp},
    };
    for channels in [1, 2, 6, 26] {
        let (file, expected) = fixture(16, false, channels, 17);
        let source =
            MediaSourceStream::new(Box::new(Cursor::new(file.clone())), Default::default());
        let format = WavReader::try_new(source, FormatOptions::default()).unwrap();
        let mut decoder = PcmDecoder::try_new(
            format.tracks()[0]
                .codec_params
                .as_ref()
                .unwrap()
                .audio()
                .unwrap(),
            &AudioDecoderOptions::default(),
        )
        .unwrap();
        let data = &file[file.len() - 17 * channels as usize * 2..];
        let packet = PacketRef::new(0, Timestamp::ZERO, Duration::from(17_u64), data);
        let block = decoder.decode_ref(&packet).unwrap();
        assert_eq!(block.samples_interleaved(), 17 * channels as usize);
        let mut out = vec![0.0; 17 * channels as usize];
        block.copy_to_slice_interleaved(&mut out);
        assert_eq!(out, expected);
        let partial = PacketRef::new(
            0,
            Timestamp::ZERO,
            Duration::from(17_u64),
            &data[..data.len() - 1],
        );
        assert!(
            decoder.decode_ref(&partial).is_err(),
            "partial coded frame silently accepted"
        );
        assert_eq!(
            decoder.last_decoded().samples_interleaved(),
            0,
            "error kept the previous packet visible"
        );
        assert_eq!(
            decoder.decode_ref(&packet).unwrap().samples_interleaved(),
            17 * channels as usize
        );
    }
}

#[test]
fn pcm_small_packet_decode_and_convert_has_no_steady_rust_allocation() {
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    use symphonia_core::{
        packet::PacketRef,
        units::{Duration, Timestamp},
    };
    for channels in [1, 2, 6, 26] {
        let (file, _) = fixture(16, false, channels, 17);
        let source =
            MediaSourceStream::new(Box::new(Cursor::new(file.clone())), Default::default());
        let format = WavReader::try_new(source, FormatOptions::default()).unwrap();
        let mut decoder = PcmDecoder::try_new(
            format.tracks()[0]
                .codec_params
                .as_ref()
                .unwrap()
                .audio()
                .unwrap(),
            &AudioDecoderOptions::default(),
        )
        .unwrap();
        let data = &file[file.len() - 17 * channels as usize * 2..];
        let packet = PacketRef::new(0, Timestamp::ZERO, Duration::from(17_u64), data);
        let mut out = vec![0.0; 17 * channels as usize];
        assert_no_steady_state_allocation(
            Burst {
                path: "typed PCM short packet plus interleaved conversion, up to26channels",
                warmup: 32,
                measured: 256,
            },
            |_| {
                let block = decoder.decode_ref(&packet).unwrap();
                assert_eq!(block.samples_interleaved(), out.len());
                block.copy_to_slice_interleaved(&mut out);
            },
        );
    }
}

#[test]
fn pcm_profile_maximum_32_channel_plane_views_do_not_allocate() {
    use migo_alloc_probe::{Burst, assert_no_steady_state_allocation};
    use symphonia_core::{
        audio::Channels,
        codecs::audio::{AudioCodecParameters, well_known::CODEC_ID_PCM_S16LE},
        packet::PacketRef,
        units::{Duration, Timestamp},
    };
    let mut params = AudioCodecParameters::new();
    params
        .for_codec(CODEC_ID_PCM_S16LE)
        .with_sample_rate(48000)
        .with_channels(Channels::Discrete(32))
        .with_max_frames_per_packet(1152);
    let mut decoder = PcmDecoder::try_new(&params, &AudioDecoderOptions::default()).unwrap();
    let input = vec![0_u8; 17 * 32 * 2];
    let mut out = vec![0_f32; 17 * 32];
    let packet = PacketRef::new(0, Timestamp::ZERO, Duration::from(17_u64), &input);
    assert_no_steady_state_allocation(
        Burst {
            path: "PCM32channels small packet and interleaved conversion",
            warmup: 32,
            measured: 256,
        },
        |_| {
            let block = decoder.decode_ref(&packet).unwrap();
            block.copy_to_slice_interleaved(&mut out);
        },
    );
    assert!(out.iter().all(|sample| *sample == 0.0));
}
