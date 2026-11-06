//! Pinned-source Vorbis uses the original packet codec, buffers and AudioCpu.
use super::*;
use crate::streaming_codecs::packet::PacketStream;
use crate::streaming_transport::{Decoder, Progress};
use std::sync::atomic::AtomicBool;

type Stream = PacketStream<Vorbis>;
struct Rendered {
    pcm: Vec<f32>,
    decoder: Stream,
    requests: Vec<u64>,
}
fn render(data: &[u8], rate: u32) -> EngineResult<Rendered> {
    render_chunk(data, rate, 8192)
}
fn render_chunk(data: &[u8], rate: u32, chunk: usize) -> EngineResult<Rendered> {
    let mut decoder = Stream::vorbis(rate, data.len() as u64);
    let mut position = 0;
    let mut pcm = Vec::new();
    let mut output = Vec::with_capacity(32768);
    let mut requests = Vec::new();
    for _ in 0..1_000_000 {
        output.clear();
        let end = (position + chunk).min(data.len());
        let step = decoder.step(
            &data[position..end],
            position == data.len(),
            &mut output,
            &AtomicBool::new(false),
        )?;
        position += step.consumed;
        if step.sample_rate != 0 {
            assert!(
                decoder.timeline()?.is_some(),
                "Vorbis advertised format before its pinned source clock"
            );
        }
        pcm.extend_from_slice(&output);
        if step.progress == Progress::NeedSource {
            let offset = decoder
                .vorbis_request()
                .expect("Vorbis hid its real source request");
            requests.push(offset);
            decoder.resume_vorbis(offset)?;
            position = usize::try_from(offset).unwrap();
        }
        if step.progress == Progress::Finished {
            return Ok(Rendered {
                pcm,
                decoder,
                requests,
            });
        }
    }
    panic!("Vorbis replay exceeded its deterministic step budget")
}
#[test]
fn fragmented_source_restore_waits_for_the_complete_first_audio_page() {
    for data in [
        include_bytes!("decoder/ogg-fixtures/cross.ogg").as_slice(),
        include_bytes!("decoder/ogg-fixtures/max-window-8.ogg").as_slice(),
    ] {
        let expected = crate::decoder::decode_for_rate(data, 16000).unwrap();
        for chunk in [1, 137, 4096] {
            let got = render_chunk(data, 16000, chunk)
                .expect("fragmented Vorbis source restore lost its pending page");
            assert_eq!(
                got.pcm.iter().map(|n| n.to_bits()).collect::<Vec<_>>(),
                expected
                    .samples
                    .iter()
                    .map(|n| n.to_bits())
                    .collect::<Vec<_>>(),
                "fragmented Vorbis source restore changed complete PCM"
            );
        }
    }
}
fn fixtures() -> Vec<(&'static str, &'static [u8])> {
    vec![
        ("tiny", include_bytes!("decoder/ogg-fixtures/0.ogg")),
        ("stereo", include_bytes!("decoder/ogg-fixtures/1.ogg")),
        ("mono", include_bytes!("decoder/ogg-fixtures/2.ogg")),
        ("cross", include_bytes!("decoder/ogg-fixtures/cross.ogg")),
        (
            "wide",
            include_bytes!("decoder/ogg-fixtures/max-window-8.ogg"),
        ),
    ]
}
fn dump(pcm: &[f32], key: &str) {
    if let Some(dir) = std::env::var_os("MIGO_VORBIS_CONTROL_PCM_DIR") {
        std::fs::write(
            std::path::Path::new(&dir).join(format!("native-{key}.f32le")),
            pcm.iter().flat_map(|n| n.to_le_bytes()).collect::<Vec<_>>(),
        )
        .unwrap();
    }
}
#[test]
fn source_probe_and_replay_keep_complete_mature_pcm_and_exact_native_clock() {
    for (name, data) in fixtures() {
        let native = crate::decoder::decode(data).unwrap();
        for rate in [3000, 16000, 48000] {
            let got = render(data, rate).unwrap();
            let want = crate::decoder::decode_for_rate(data, rate).unwrap();
            assert!(
                got.pcm.len() == want.samples.len()
                    && got
                        .pcm
                        .iter()
                        .zip(&want.samples)
                        .all(|(a, b)| a.to_bits() == b.to_bits()),
                "retained Vorbis complete PCM differs: {name}/{rate}"
            );
            let clock = got.decoder.timeline().unwrap().unwrap();
            assert_eq!(
                clock.input_frames(),
                (native.samples.len() / native.channels as usize) as u64,
                "Vorbis probe changed the complete source clock"
            );
            assert_eq!(
                clock.output_frames() as usize * native.channels as usize,
                got.pcm.len()
            );
            assert!(
                got.requests.is_empty() || got.requests.len() == 2,
                "Vorbis scanned the entire source instead of probing its tail"
            );
            dump(&got.pcm, &format!("{name}-{rate}"));
        }
    }
}
#[test]
fn consecutive_cutovers_keep_the_validated_clock_while_headers_are_replayed() {
    let data = include_bytes!("decoder/ogg-fixtures/1.ogg");
    let mut got = render(data, 16000).unwrap();
    got.decoder.reposition_vorbis(300, None).unwrap();
    assert!(
        got.decoder.reposition_vorbis(211, None).is_ok(),
        "second Vorbis cutover lost its validated clock before replay headers"
    );
    assert!(got.decoder.reposition_vorbis(-1, None).is_err());
    assert!(
        got.decoder.reposition_vorbis(0, None).is_ok(),
        "rejected Vorbis target changed its retained owner"
    );
}
fn checksum(data: &mut [u8]) {
    let mut value = 0_u32;
    for (i, &b) in data.iter().enumerate() {
        value ^= u32::from(if (22..26).contains(&i) { 0 } else { b }) << 24;
        for _ in 0..8 {
            value = if value & 0x80000000 != 0 {
                (value << 1) ^ 0x04c11db7
            } else {
                value << 1
            };
        }
    }
    data[22..26].copy_from_slice(&value.to_le_bytes());
}
fn pages(data: &[u8]) -> Vec<std::ops::Range<usize>> {
    // Test-only independent fixture surgery. Production uses the library CRC parser.
    let mut result = Vec::new();
    let mut at = 0;
    while at < data.len() {
        assert_eq!(&data[at..at + 4], b"OggS");
        let n = data[at + 26] as usize;
        let end = at
            + 27
            + n
            + data[at + 27..at + 27 + n]
                .iter()
                .map(|b| *b as usize)
                .sum::<usize>();
        result.push(at..end);
        at = end;
    }
    result
}
#[test]
fn nonzero_page_origin_is_normalized_without_changing_complete_pcm() {
    let original = include_bytes!("decoder/ogg-fixtures/max-window-8.ogg");
    let mut shifted = original.to_vec();
    for page in pages(original).into_iter().skip(2) {
        let p = &mut shifted[page];
        let gp = u64::from_le_bytes(p[6..14].try_into().unwrap());
        p[6..14].copy_from_slice(&(gp + 8192).to_le_bytes());
        checksum(p);
    }
    let expected = crate::decoder::decode_for_rate(&shifted, 16000).unwrap();
    let got = render(&shifted, 16000).unwrap();
    assert_eq!(
        got.decoder.timeline().unwrap().unwrap().input_frames(),
        8192,
        "Vorbis granule origin was counted as playable PCM"
    );
    assert!(
        got.pcm
            .iter()
            .zip(&expected.samples)
            .all(|(a, b)| a.to_bits() == b.to_bits())
            && got.pcm.len() == expected.samples.len()
    );
    dump(&got.pcm, "shifted-16000");
}
#[test]
fn malformed_final_page_never_advertises_a_successful_presentation() {
    let original = include_bytes!("decoder/ogg-fixtures/max-window-8.ogg");
    let last = pages(original).pop().unwrap();
    for kind in 0..6 {
        let mut data = original.to_vec();
        match kind {
            0 => data[last.start + 22] ^= 1,
            1 => {
                data[last.start + 14] ^= 1;
                checksum(&mut data[last.clone()]);
            }
            2 => {
                data[last.start + 5] &= !4;
                checksum(&mut data[last.clone()]);
            }
            3 => {
                data[last.start + 6..last.start + 14].copy_from_slice(&u64::MAX.to_le_bytes());
                checksum(&mut data[last.clone()]);
            }
            4 => data.push(0),
            _ => data.extend_from_slice(original),
        }
        assert!(
            render(&data, 16000).is_err(),
            "invalid final Vorbis page produced successful PCM: kind={kind}"
        );
    }
}

#[test]
fn hostile_tail_verifies_only_one_crc_candidate_per_original_cpu_step() {
    let data = include_bytes!("decoder/ogg-fixtures/max-window-8.ogg");
    let mut d = Stream::vorbis(16000, data.len() as u64 + 65536);
    let mut at = 0;
    let mut out = Vec::with_capacity(32768);
    for _ in 0..1000 {
        out.clear();
        let step = d
            .step(&data[at..], false, &mut out, &AtomicBool::new(false))
            .unwrap();
        at += step.consumed;
        if step.progress == Progress::NeedSource {
            break;
        }
    }
    let offset = d
        .vorbis_request()
        .expect("metadata probe never requested its original source");
    d.resume_vorbis(offset).unwrap();
    let before = d.owner.as_ref().unwrap().decoder.discarded_bytes();
    let mut invalid = vec![0; 65307];
    invalid[..5].copy_from_slice(b"OggS\0");
    out.clear();
    let step = d
        .step(&invalid, false, &mut out, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(step.progress, Progress::Yield);
    assert_eq!(
        d.owner.as_ref().unwrap().decoder.discarded_bytes() - before,
        1,
        "one Vorbis CPU step retried multiple invalid CRC candidates"
    );
    assert!(out.is_empty());
}

#[tokio::test]
async fn original_cpu_really_destroys_all_retained_vorbis_buffer_capacity() {
    use crate::off_worker::OffWorker;
    use std::time::Duration;
    let host = migo_io::scheduler::IoScheduler::new(1121);
    let peer = migo_io::scheduler::IoScheduler::new(1122);
    let slot = OffWorker::new(None::<Stream>, host.audio_cpu());
    let (slot, watches) = slot
        .with(|slot| {
            let got = render(include_bytes!("decoder/ogg-fixtures/2.ogg"), 16000).unwrap();
            let watches = got.decoder.watches();
            assert_eq!(watches.len(), 3);
            assert!(got.decoder.owner.as_ref().unwrap().decoder.input_len() < watches[0].1);
            *slot = Some(got.decoder);
            watches
        })
        .await
        .unwrap();
    let (entered, started) = std::sync::mpsc::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let bytes = watches.iter().map(|(_, n)| *n).sum::<usize>();
    let task = tokio::spawn(slot.with(move |slot| {
        entered.send(()).unwrap();
        blocked.recv_timeout(Duration::from_secs(15)).unwrap();
        let mut out = Vec::with_capacity(32768);
        assert!(
            std::thread::current()
                .name()
                .unwrap_or("")
                .starts_with("Migo-IO-")
        );
        let before = migo_alloc_probe::thread_counts().bytes_freed;
        assert_eq!(
            slot.as_mut()
                .unwrap()
                .step(&[], false, &mut out, &AtomicBool::new(true))
                .unwrap()
                .progress,
            Progress::Cancelled
        );
        assert!(
            migo_alloc_probe::thread_counts().bytes_freed - before >= bytes as u64,
            "original Vorbis CPU did not really free its full buffer capacity"
        );
    }));
    tokio::task::spawn_blocking(move || started.recv_timeout(Duration::from_secs(15)).unwrap())
        .await
        .unwrap();
    assert!(
        watches.iter().all(|(w, _)| w.freed_bytes().is_none()),
        "Vorbis capacity freed before its original CPU returned"
    );
    assert_eq!(peer.audio_cpu().run(|| 17).await.unwrap(), 17);
    release.send(()).unwrap();
    let (slot, ()) = task.await.unwrap().unwrap();
    assert!(
        watches.iter().all(|(w, n)| w.freed_bytes() == Some(*n)),
        "original CPU did not destroy retained Vorbis capacity"
    );
    drop(slot);
    host.close();
    peer.close();
}

pub(crate) fn silence(packets: usize, padded_bytes: usize) -> Vec<u8> {
    assert!((2..=16384).contains(&padded_bytes));
    let template = include_bytes!("decoder/ogg-fixtures/max-window-8.ogg");
    let parts = pages(template);
    let mut result = template[..parts[2].end].to_vec();
    let header = &template[parts[3].clone()][..27];
    let segments = padded_bytes / 255 + 1;
    let per_page = 255 / segments;
    let mut done = 0;
    let mut sequence = 3_u32;
    while done < packets {
        let count = (packets - done).min(per_page);
        let mut page = header.to_vec();
        page[5] = if done + count == packets { 4 } else { 0 };
        page[6..14].copy_from_slice(&(((done + count) as u64) * 4096).to_le_bytes());
        page[18..22].copy_from_slice(&sequence.to_le_bytes());
        page[26] = (count * segments) as u8;
        for _ in 0..count {
            page.extend(std::iter::repeat_n(255_u8, padded_bytes / 255));
            page.push((padded_bytes % 255) as u8);
        }
        let payload = page.len();
        page.resize(payload + count * padded_bytes, 0);
        for i in 0..count {
            page[payload + i * padded_bytes] = 0x0e;
        }
        checksum(&mut page);
        result.extend(page);
        done += count;
        sequence += 1;
    }
    if let Some(dir) = std::env::var_os("MIGO_VORBIS_FIXTURE_DIR") {
        std::fs::write(
            std::path::Path::new(&dir).join(format!("silence-{packets}-{padded_bytes}.ogg")),
            &result,
        )
        .unwrap();
    }
    result
}
