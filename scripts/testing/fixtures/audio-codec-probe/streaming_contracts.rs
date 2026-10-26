use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::streaming::{Decoder, ENCODED_CAPACITY, Progress, SAMPLES_PER_STEP};
use symphonia_bundle_mp3::streaming::{MpaPacketizer, PacketStep, PacketizerOptions};

#[global_allocator]
static COUNTING: migo_alloc_probe::CountingAllocator =
    migo_alloc_probe::CountingAllocator::system();

fn fixture(name: &str) -> Vec<u8> {
    fs::read(
        PathBuf::from(std::env::var_os("W09_STREAM_FIXTURES").expect("generated fixtures"))
            .join(name),
    )
    .unwrap()
}

fn drain(
    decoder: &mut Decoder,
    out: &mut Vec<f32>,
    all: &mut Vec<f32>,
) -> Result<Progress, String> {
    loop {
        out.clear();
        let result = decoder.drain(out, &AtomicBool::new(false))?;
        assert!(out.len() <= SAMPLES_PER_STEP);
        all.extend_from_slice(out);
        if result != Progress::Yield {
            return Ok(result);
        }
    }
}

fn decode(data: &[u8], chunk: usize, cap: usize) -> Result<Vec<f32>, String> {
    let mut decoder = Decoder::new(Some(cap as u64));
    let mut out = Vec::with_capacity(SAMPLES_PER_STEP);
    let mut all = Vec::new();
    for input in data.chunks(chunk) {
        let mut pos = 0;
        while pos < input.len() {
            let accepted = decoder.push(&input[pos..])?;
            assert!(accepted != 0, "bounded feed made no progress");
            pos += accepted;
            assert_eq!(
                drain(&mut decoder, &mut out, &mut all)?,
                Progress::NeedInput
            );
        }
    }
    decoder.finish_input();
    assert_eq!(drain(&mut decoder, &mut out, &mut all)?, Progress::Finished);
    assert_eq!(
        decoder.drain(
            &mut Vec::with_capacity(SAMPLES_PER_STEP),
            &AtomicBool::new(false)
        )?,
        Progress::Finished
    );
    assert!(decoder.push(&[0]).is_err());
    Ok(all)
}

#[test]
fn arbitrary_chunks_and_eof_equal_the_whole_file_decoder() {
    for name in [
        "cbr-44100-1.mp3",
        "vbr-48000-2.mp3",
        "mpeg2-22050-1.mp3",
        "mpeg25-8000-2.mp3",
        "no-xing.mp3",
        "no-xing-vbr.mp3",
        "tiny.mp3",
    ] {
        let data = fixture(name);
        let whole = crate::candidate::decode(&data, 16_777_216).unwrap();
        for chunk in [1, 2, 3, 4, 7, 31, 257, 4096, 65536, 100000] {
            assert_eq!(
                decode(&data, chunk, whole.samples.len()).unwrap(),
                whole.samples,
                "chunk={chunk}, {name}"
            );
        }
    }
}

fn syncsafe(value: usize) -> [u8; 4] {
    [
        ((value >> 21) & 127) as u8,
        ((value >> 14) & 127) as u8,
        ((value >> 7) & 127) as u8,
        (value & 127) as u8,
    ]
}

#[test]
fn metadata_payload_and_footer_never_become_audio_frames() {
    let original = fixture("cbr-44100-1.mp3");
    let expected = crate::candidate::decode(&original, 16_777_216)
        .unwrap()
        .samples;
    for version in [3, 4] {
        let mut private = b"migo.test\0".to_vec();
        private.extend_from_slice(&original);
        let mut body = b"PRIV".to_vec();
        body.extend_from_slice(&if version == 4 {
            syncsafe(private.len())
        } else {
            (private.len() as u32).to_be_bytes()
        });
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(&private);
        let mut header = vec![
            b'I',
            b'D',
            b'3',
            version,
            0,
            if version == 4 { 0x10 } else { 0 },
        ];
        header.extend_from_slice(&syncsafe(body.len()));
        let mut data = vec![0; 31];
        data.extend_from_slice(&header);
        data.extend_from_slice(&body);
        if version == 4 {
            header[..3].copy_from_slice(b"3DI");
            data.extend_from_slice(&header);
        }
        data.extend_from_slice(&original);
        let mut tail = vec![0; 128];
        tail[..3].copy_from_slice(b"TAG");
        tail[8..12].copy_from_slice(&[0xff, 0xfb, 0x90, 0]);
        data.extend_from_slice(&tail);
        for chunk in [1, 3, 10, 257, 65536] {
            assert_eq!(decode(&data, chunk, expected.len()).unwrap(), expected);
        }
    }
}

#[test]
fn id3_footer_is_in_the_metadata_extent() {
    let mut data = b"ID3\x04\x00\x10\x00\x00\x00\x03".to_vec();
    data.extend_from_slice(&[0; 3]);
    data.extend_from_slice(b"3DI\x04\x00\x10\x00\x00\x00\x03");
    data.extend_from_slice(&fixture("cbr-44100-1.mp3"));
    let mut packets = MpaPacketizer::new(PacketizerOptions {
        max_junk_bytes: 0,
        ..Default::default()
    });
    assert!(matches!(
        packets.next(&data, false).unwrap(),
        PacketStep::Discard(23)
    ));
}

#[test]
fn packets_borrow_the_supplied_input_and_preserve_valid_timing() {
    let data = fixture("cbr-44100-1.mp3");
    let mut packets = MpaPacketizer::new(PacketizerOptions::default());
    let mut offset = 0;
    let mut total = 0;
    loop {
        match packets.next(&data[offset..], true).unwrap() {
            PacketStep::Packet { consumed, packet } => {
                assert_eq!(packet.data.as_ptr(), data[offset..].as_ptr());
                assert_eq!(packet.data.len(), consumed);
                assert_eq!(packet.block_dur().get(), 1152);
                total += packet.dur.get();
                offset += consumed;
            }
            PacketStep::Discard(bytes) => {
                assert!(bytes != 0);
                offset += bytes;
            }
            PacketStep::End => break,
            PacketStep::NeedMore => panic!("EOF must resolve incomplete input"),
        }
    }
    assert_eq!(total, packets.track().unwrap().num_frames.unwrap());
    assert!(packets.track().unwrap().time_base.is_some());
}

#[test]
fn malformed_truncated_and_over_budget_inputs_return_errors() {
    let valid = fixture("cbr-44100-1.mp3");
    let mut truncated = valid.clone();
    truncated.pop();
    let mut overclaimed = b"ID3\x04\x00\x00".to_vec();
    overclaimed.extend_from_slice(&[0x7f; 4]);
    for data in [
        vec![],
        b"ID3".to_vec(),
        overclaimed,
        vec![0; 65537],
        truncated,
    ] {
        assert!(decode(&data, 257, 16_777_216).is_err());
    }
    let whole = crate::candidate::decode(&valid, 16_777_216).unwrap();
    assert!(decode(&valid, 257, whole.samples.len() - 1).is_err());
    let mut decoder = Decoder::new(None);
    decoder.push(b"ID3").unwrap();
    decoder.finish_input();
    let mut out = Vec::with_capacity(SAMPLES_PER_STEP);
    assert!(decoder.drain(&mut out, &AtomicBool::new(false)).is_err());
    assert!(decoder.push(b"more").is_err());
    assert!(decoder.drain(&mut out, &AtomicBool::new(false)).is_err());
    let mut tag = b"ID3\x04\x00\x00".to_vec();
    tag.extend_from_slice(&syncsafe(70_000));
    tag.resize(70_010, 0);
    tag.extend_from_slice(&valid);
    assert!(
        decode(&tag, 257, 16_777_216)
            .unwrap_err()
            .contains("metadata byte budget exceeded")
    );
    let mut junk = vec![0; 65537];
    junk.extend_from_slice(&valid);
    assert!(
        decode(&junk, 257, 16_777_216)
            .unwrap_err()
            .contains("synchronization byte budget exceeded")
    );
    let mut limited = Decoder::new(Some(1500));
    limited.push(&fixture("no-xing.mp3")).unwrap();
    out.clear();
    assert!(limited.drain(&mut out, &AtomicBool::new(false)).is_err());
    assert!(out.is_empty(), "failed step exposed its partial PCM");
}

#[test]
fn input_and_work_are_bounded_and_cancellation_is_sticky() {
    let data = fixture("long.mp3");
    let mut decoder = Decoder::new(None);
    assert_eq!(
        decoder.push(&data).unwrap(),
        ENCODED_CAPACITY.min(data.len())
    );
    let mut out = Vec::with_capacity(SAMPLES_PER_STEP);
    let cancel = AtomicBool::new(false);
    assert_eq!(decoder.drain(&mut out, &cancel).unwrap(), Progress::Yield);
    assert!(!out.is_empty() && out.len() <= SAMPLES_PER_STEP);
    cancel.store(true, Ordering::Release);
    out.clear();
    assert_eq!(
        decoder.drain(&mut out, &cancel).unwrap(),
        Progress::Cancelled
    );
    assert!(out.is_empty());
    cancel.store(false, Ordering::Release);
    assert_eq!(
        decoder.drain(&mut out, &cancel).unwrap(),
        Progress::Cancelled
    );
    assert!(decoder.push(&[0]).is_err());
}

#[test]
fn steady_push_packetize_decode_convert_has_no_heap_allocation() {
    let data = fixture("long.mp3");
    assert!(data.len() > 32768);
    let mut decoder = Decoder::new(None);
    let mut out = Vec::with_capacity(SAMPLES_PER_STEP);
    let cancel = AtomicBool::new(false);
    decoder.push(&data[..8192]).unwrap();
    loop {
        out.clear();
        if decoder.drain(&mut out, &cancel).unwrap() != Progress::Yield {
            break;
        }
    }
    let mut offset = 8192;
    migo_alloc_probe::assert_no_steady_state_allocation(
        migo_alloc_probe::Burst {
            path: "W09 push MP3 packetize/decode/convert",
            warmup: 2,
            measured: 32,
        },
        |_| {
            let end = offset + 511;
            assert_eq!(decoder.push(&data[offset..end]).unwrap(), 511);
            offset = end;
            loop {
                out.clear();
                let result = decoder.drain(&mut out, &cancel).unwrap();
                std::hint::black_box(&out);
                if result != Progress::Yield {
                    assert_eq!(result, Progress::NeedInput);
                    break;
                }
            }
        },
    );
}

#[test]
fn long_music_has_resident_limits_without_a_default_duration_limit() {
    let data = fixture("duration-over-cache-cap.mp3");
    let mut unlimited = Decoder::new(None);
    let mut limited = Decoder::new(Some(crate::limits::MAX_AUDIO_PCM_SAMPLES));
    let mut out = Vec::with_capacity(SAMPLES_PER_STEP);
    unlimited.push(&data).unwrap();
    assert_eq!(
        unlimited.drain(&mut out, &AtomicBool::new(false)).unwrap(),
        Progress::Yield
    );
    assert!(out.len() <= SAMPLES_PER_STEP && !out.is_empty());
    limited.push(&data).unwrap();
    out.clear();
    assert!(
        limited
            .drain(&mut out, &AtomicBool::new(false))
            .unwrap_err()
            .contains("declared stream exceeds PCM budget")
    );
}
