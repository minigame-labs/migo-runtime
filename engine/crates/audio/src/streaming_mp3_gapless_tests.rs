use super::*;

const MUSIC: &[u8] = include_bytes!("decoder/mp3/fixtures/music.mp3");

#[test]
fn complete_gapless_pcm_matches_frozen_presentation_and_independent_sinc() {
    for (name, encoded, frozen, rate, channels) in [
        (
            "music",
            MUSIC,
            include_bytes!("decoder/mp3/fixtures/music.i16le").as_slice(),
            48000,
            2,
        ),
        (
            "tiny-mono",
            include_bytes!("decoder/mp3/fixtures/tiny-47-mono.mp3").as_slice(),
            include_bytes!("decoder/mp3/fixtures/tiny-47-mono.i16le").as_slice(),
            48000,
            1,
        ),
        (
            "tiny-stereo",
            include_bytes!("decoder/mp3/fixtures/tiny-47-stereo.mp3").as_slice(),
            include_bytes!("decoder/mp3/fixtures/tiny-47-stereo.i16le").as_slice(),
            44100,
            2,
        ),
        (
            "mpeg2",
            include_bytes!("decoder/mp3/fixtures/gapless-mpeg2.mp3").as_slice(),
            include_bytes!("decoder/mp3/fixtures/gapless-mpeg2.i16le").as_slice(),
            22050,
            1,
        ),
    ] {
        for target in [8000, 16000, 22050, 44100, 48000, 96000] {
            let source = DecodedAudio {
                samples: frozen
                    .chunks_exact(2)
                    .map(|v| f32::from(i16::from_le_bytes(v.try_into().unwrap())) / 32768.0)
                    .collect(),
                sample_rate: rate,
                channels,
            };
            let expected = crate::resample_job::tests::reference(&source, target);
            for partition in [1, 137, 4096, 65536] {
                let actual = collect(encoded, target, partition);
                assert_eq!(
                    actual.len(),
                    expected.len(),
                    "gapless stream complete PCM differs from frozen oracle"
                );
                assert!(
                    actual
                        .iter()
                        .zip(&expected)
                        .all(|(a, b)| a.to_bits() == b.to_bits()),
                    "gapless stream complete PCM differs from frozen oracle"
                );
                if let Some(dir) = std::env::var_os("MIGO_MP3_GAPLESS_PCM_DIR") {
                    let path = std::path::Path::new(&dir)
                        .join(format!("{name}-{target}-{partition}.f32le"));
                    std::fs::write(
                        path,
                        actual
                            .iter()
                            .flat_map(|x| x.to_le_bytes())
                            .collect::<Vec<_>>(),
                    )
                    .unwrap();
                }
            }
        }
    }
}

#[test]
fn authoritative_clock_is_ready_before_pcm_and_before_original_body_eof() {
    let mut d = Mp3StreamDecoder::new(44100);
    let mut pos = 0;
    let mut out = Vec::with_capacity(32768);
    let mut ready = false;
    let mut samples = 0;
    for _ in 0..1000 {
        out.clear();
        let step = checked(
            &mut d,
            &MUSIC[pos..16384],
            false,
            &mut out,
            &AtomicBool::new(false),
        )
        .unwrap();
        pos += step.consumed;
        if step.sample_rate != 0 {
            ready = true;
            let clock = d
                .timeline()
                .unwrap()
                .expect("progressive MP3 Ready lacks authoritative presentation clock");
            assert_eq!(
                clock,
                crate::streaming::StreamTimeline::new(480137, 48000, 44100, 2).unwrap(),
                "progressive MP3 clock differs from declared gapless presentation"
            );
        }
        if !out.is_empty() {
            assert!(ready);
            samples += out.len();
        }
        assert_ne!(step.progress, Progress::Finished);
        if step.progress == Progress::NeedInput && pos == 16384 {
            break;
        }
    }
    assert!(
        ready && samples > 0 && pos == 16384 && pos < MUSIC.len(),
        "gapless stream waited for original response EOF"
    );
}

#[test]
fn declared_gapless_truncation_is_terminal_and_never_publishes_done() {
    for (end, target) in [
        (MUSIC.len() / 2, 48000),
        (MUSIC.len() / 2, 16000),
        (MUSIC.len() - 1024, 48000),
        (MUSIC.len() - 1024, 16000),
    ] {
        let data = &MUSIC[..end];
        let mut d = Mp3StreamDecoder::new(target);
        let mut out = Vec::with_capacity(32768);
        let mut pos = 0;
        let mut eof = false;
        let mut failed = false;
        for _ in 0..100000 {
            out.clear();
            match checked(&mut d, &data[pos..], eof, &mut out, &AtomicBool::new(false)) {
                Err(e) => {
                    assert_eq!(
                        e.code,
                        ErrorCode::InvalidArgument,
                        "truncated MP3 must validate its native presentation before SRC flushing"
                    );
                    assert!(
                        e.detail
                            .as_deref()
                            .unwrap()
                            .contains("declared presentation"),
                        "truncated MP3 must validate its native presentation before SRC flushing"
                    );
                    assert!(
                        out.is_empty() && d.owner.is_none(),
                        "MP3 error retained owners or PCM"
                    );
                    assert_eq!(
                        d.step(&[], true, &mut out, &AtomicBool::new(false))
                            .err()
                            .unwrap()
                            .code,
                        e.code
                    );
                    failed = true;
                    break;
                }
                Ok(step) => {
                    pos += step.consumed;
                    assert_ne!(
                        step.progress,
                        Progress::Finished,
                        "truncated gapless stream published successful Done"
                    );
                    if step.progress == Progress::NeedInput && pos == data.len() {
                        eof = true;
                    }
                }
            }
        }
        assert!(failed, "truncated gapless stream did not terminate");
    }
}

#[test]
fn long_declared_stream_consumes_every_sample_with_bounded_resident_capacity() {
    use crate::mp3_fixture as f;
    let count = crate::limits::MAX_AUDIO_PCM_SAMPLES as usize / f::SAMPLES_PER_FRAME + 1;
    let mut tag = f::stream(1);
    tag[36..40].copy_from_slice(b"Info");
    tag[40..44].copy_from_slice(&1u32.to_be_bytes());
    tag[44..48].copy_from_slice(&(count as u32).to_be_bytes());
    let data = [tag, f::stream(count)].concat();
    let expected = count * f::SAMPLES_PER_FRAME;
    assert!(expected as u64 > crate::limits::MAX_AUDIO_PCM_SAMPLES);
    assert!(
        crate::decoder::decode(&data).is_err(),
        "whole-file quota was accidentally relaxed"
    );
    let mut d = Mp3StreamDecoder::new(f::SAMPLE_RATE);
    let mut out = Vec::with_capacity(32768);
    let mut pos = 0;
    let mut eof = false;
    let mut samples = 0;
    let mut done = false;
    for _ in 0..1000000 {
        out.clear();
        let step = checked(&mut d, &data[pos..], eof, &mut out, &AtomicBool::new(false))
            .expect("progressive MP3 imposed a whole-file duration ceiling");
        pos += step.consumed;
        assert!(
            out.iter().all(|x| *x == 0.0),
            "long synthetic stream PCM changed"
        );
        samples += out.len();
        if let Some(o) = &d.owner {
            assert_eq!(o.buffer.capacity(), RECEIVE_BYTES);
            assert_eq!(o.scratch.capacity(), MAX_SAMPLES_PER_FRAME);
            assert_eq!(o.output.capacity(), 2 * STEP_SAMPLES);
        }
        if step.progress == Progress::Finished {
            done = true;
            break;
        }
        if step.progress == Progress::NeedInput && pos == data.len() {
            eof = true;
        }
    }
    assert!(done && eof && pos == data.len());
    assert_eq!(
        samples, expected,
        "long declared MP3 lost or repeated presentation samples"
    );
    assert_eq!(
        d.timeline().unwrap(),
        Some(
            crate::streaming::StreamTimeline::new(
                (expected / 2) as u64,
                f::SAMPLE_RATE,
                f::SAMPLE_RATE,
                2
            )
            .unwrap()
        )
    );
    println!(
        "MIGO_MP3_GAPLESS_LARGE samples={samples} encoded={} complete_zero_pcm_checked=true",
        data.len()
    );
}

#[test]
fn free_format_gapless_framing_and_bootstrap_cancellation_keep_ownership() {
    for length in [417, 1000, 2303] {
        let mut free = Vec::new();
        for original in crate::mp3_fixture::stream(7).chunks_exact(crate::mp3_fixture::FRAME_BYTES)
        {
            let mut frame = original.to_vec();
            frame[2] &= 0x0f;
            frame.resize(length, 0);
            free.extend(frame);
        }
        let expected = crate::decoder::decode(&free).unwrap();
        for size in [1, 137, 4096, 65536] {
            assert_eq!(
                collect(&free, expected.sample_rate, size),
                expected.samples,
                "progressive free-format presentation changed"
            );
        }
    }
    for stop in 0..40 {
        let mut d = Mp3StreamDecoder::new(16000);
        let mut out = Vec::with_capacity(32768);
        let mut pos = 0;
        for _ in 0..stop {
            out.clear();
            let step = checked(
                &mut d,
                &MUSIC[pos..],
                false,
                &mut out,
                &AtomicBool::new(false),
            )
            .unwrap();
            pos += step.consumed;
        }
        out.push(0.5);
        let step = checked(&mut d, &[], false, &mut out, &AtomicBool::new(true)).unwrap();
        assert_eq!(step.progress, Progress::Cancelled);
        assert!(
            out.is_empty() && d.owner.is_none(),
            "gapless bootstrap cancellation retained native owner"
        );
    }
}

#[tokio::test]
async fn actual_spare_capacities_stay_owned_until_original_cpu_cancellation() {
    use crate::off_worker::OffWorker;
    use std::sync::Arc;
    let host = migo_io::scheduler::IoScheduler::new(1069);
    let peer = migo_io::scheduler::IoScheduler::new(1070);
    let slot = OffWorker::new(None::<Mp3StreamDecoder>, host.audio_cpu());
    let (slot, watches) = slot
        .with(|slot| {
            let mut d = Mp3StreamDecoder::new(16000);
            let mut out = Vec::with_capacity(32768);
            let mut pos = 0;
            for _ in 0..1000 {
                out.clear();
                let step = d
                    .step(&MUSIC[pos..16384], false, &mut out, &AtomicBool::new(false))
                    .unwrap();
                pos += step.consumed;
                if step.sample_rate != 0 {
                    break;
                }
            }
            let o = d.owner.as_ref().unwrap();
            assert!(
                o.buffer.len() < o.buffer.capacity()
                    && o.scratch.len() < o.scratch.capacity()
                    && o.output.len() < o.output.capacity()
            );
            let watches = [
                (o.buffer.as_ptr(), o.buffer.capacity()),
                (o.scratch.as_ptr().cast(), o.scratch.capacity() * 4),
                (o.output.as_ptr().cast(), o.output.capacity() * 4),
            ]
            .into_iter()
            .map(|(p, n)| (migo_alloc_probe::DeallocationWatch::new(p).unwrap(), n))
            .collect::<Vec<_>>();
            *slot = Some(d);
            watches
        })
        .await
        .unwrap();
    let (started, entered) = std::sync::mpsc::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let bytes = watches.iter().map(|(_, n)| *n as u64).sum::<u64>();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let task = tokio::spawn(slot.with(move |slot| {
        started.send(()).unwrap();
        blocked
            .recv_timeout(std::time::Duration::from_secs(15))
            .unwrap();
        let mut out = Vec::with_capacity(32768);
        let before = migo_alloc_probe::thread_counts();
        assert_eq!(
            slot.as_mut()
                .unwrap()
                .step(&[], false, &mut out, &flag)
                .unwrap()
                .progress,
            Progress::Cancelled
        );
        assert!(
            migo_alloc_probe::thread_counts().bytes_freed - before.bytes_freed >= bytes,
            "original CPU did not really free progressive MP3 capacity"
        );
        assert!(
            std::thread::current()
                .name()
                .unwrap_or("")
                .starts_with("Migo-IO-")
        );
    }));
    tokio::task::spawn_blocking(move || {
        entered
            .recv_timeout(std::time::Duration::from_secs(15))
            .unwrap()
    })
    .await
    .unwrap();
    cancel.store(true, Ordering::Release);
    let held = watches.iter().all(|(w, _)| w.freed_bytes().is_none());
    assert_eq!(peer.audio_cpu().run(|| 17).await.unwrap(), 17);
    release.send(()).unwrap();
    let (slot, ()) = task.await.unwrap().unwrap();
    assert!(held, "blocked original CPU prematurely freed MP3 capacity");
    assert!(
        watches.iter().all(|(w, n)| w.freed_bytes() == Some(*n)),
        "original CPU did not destroy every actual MP3 allocation"
    );
    drop(slot);
    host.close();
    peer.close();
}

#[test]
fn incremental_id3_skip_ignores_embedded_sync_and_never_retains_the_whole_tag() {
    let body = 100000usize;
    let mut tag = b"ID3\x04\x00\x10\0\0\0\0".to_vec();
    for i in 0..4 {
        tag[6 + i] = ((body >> (7 * (3 - i))) & 127) as u8;
    }
    tag.resize(10 + body + 10, 0);
    let false_frame = crate::mp3_fixture::stream(1);
    tag[1000..1000 + false_frame.len()].copy_from_slice(&false_frame);
    let mut d = Mp3StreamDecoder::new(48000);
    let mut out = Vec::with_capacity(32768);
    let mut pos = 0;
    for _ in 0..1000 {
        out.clear();
        let step = checked(
            &mut d,
            &tag[pos..8192],
            false,
            &mut out,
            &AtomicBool::new(false),
        )
        .expect("ID3 metadata was decoded as an audio prefix");
        pos += step.consumed;
        assert!(
            step.sample_rate == 0 && out.is_empty(),
            "ID3 metadata published audio or Ready"
        );
        if step.progress == Progress::NeedInput && pos == 8192 {
            break;
        }
    }
    assert!(
        pos == 8192 && d.owner.as_ref().unwrap().buffer.is_empty(),
        "ID3 prefix was not skipped as bounded metadata"
    );
    let data = [tag.as_slice(), MUSIC].concat();
    let samples = include_bytes!("decoder/mp3/fixtures/music.i16le")
        .chunks_exact(2)
        .map(|v| f32::from(i16::from_le_bytes(v.try_into().unwrap())) / 32768.0)
        .collect::<Vec<_>>();
    for target in [16000, 48000] {
        let expected = crate::resample_job::tests::reference(
            &DecodedAudio {
                samples: samples.clone(),
                sample_rate: 48000,
                channels: 2,
            },
            target,
        );
        for partition in [1, 137, 4096, 65536] {
            let actual = collect(&data, target, partition);
            assert_eq!(
                actual, expected,
                "ID3 metadata changed the complete gapless PCM"
            );
            if let Some(dir) = std::env::var_os("MIGO_MP3_GAPLESS_PCM_DIR") {
                std::fs::write(
                    std::path::Path::new(&dir).join(format!("id3-{target}-{partition}.f32le")),
                    actual
                        .iter()
                        .flat_map(|x| x.to_le_bytes())
                        .collect::<Vec<_>>(),
                )
                .unwrap();
            }
        }
    }
    let mut d = Mp3StreamDecoder::new(48000);
    let mut pos = 0;
    let mut eof = false;
    let mut failed = false;
    for _ in 0..1000 {
        out.clear();
        match checked(
            &mut d,
            &tag[pos..2048],
            eof,
            &mut out,
            &AtomicBool::new(false),
        ) {
            Err(e) => {
                assert!(e.detail.as_deref().unwrap().contains("ID3"));
                assert!(out.is_empty() && d.owner.is_none());
                failed = true;
                break;
            }
            Ok(step) => {
                pos += step.consumed;
                assert_eq!(
                    step.sample_rate, 0,
                    "truncated ID3 metadata published Ready"
                );
                assert!(out.is_empty());
                assert_ne!(
                    step.progress,
                    Progress::Finished,
                    "truncated ID3 metadata published Done"
                );
                if step.progress == Progress::NeedInput && pos == 2048 {
                    eof = true;
                }
            }
        }
    }
    assert!(failed, "truncated declared ID3 metadata was accepted");
}
