use super::*;
const MUSIC: &[u8] = include_bytes!("decoder/mp3/fixtures/music.mp3");
fn cases() -> Vec<(&'static str, &'static [u8])> {
    vec![
        ("music", MUSIC),
        (
            "tiny-mono",
            include_bytes!("decoder/mp3/fixtures/tiny-47-mono.mp3"),
        ),
        (
            "tiny-stereo",
            include_bytes!("decoder/mp3/fixtures/tiny-47-stereo.mp3"),
        ),
        (
            "mpeg2",
            include_bytes!("decoder/mp3/fixtures/gapless-mpeg2.mp3"),
        ),
    ]
}
fn drain(d: &mut Mp3StreamDecoder, data: &[u8], limit: Option<usize>) -> (Vec<f32>, bool) {
    if d.source_request().is_some() {
        d.resume_at(0).unwrap();
    }
    let mut eof = d.owner.as_ref().unwrap().eof;
    let mut pos = if eof { data.len() } else { 0 };
    let mut pcm = Vec::with_capacity(32768);
    let mut out = Vec::new();
    for _ in 0..1000000 {
        pcm.clear();
        let end = (pos + 137).min(data.len());
        let step = super::super::tests::checked(
            d,
            &data[pos..end],
            eof,
            &mut pcm,
            &AtomicBool::new(false),
        )
        .expect("retained MP3 failed its complete replay presentation");
        pos += step.consumed;
        out.extend_from_slice(&pcm);
        if step.progress == Progress::Finished {
            return (out, true);
        }
        if limit.is_some_and(|n| out.len() >= n) {
            return (out, false);
        }
        if step.progress == Progress::NeedSource {
            assert_eq!(d.source_request(), Some(0));
            assert!(d.resume_at(1).is_err());
            d.resume_at(0).unwrap();
            pos = 0;
            eof = false;
        } else if step.progress == Progress::NeedInput && pos == data.len() {
            eof = true;
        }
    }
    panic!("retained MP3 replay did not finish bounded steps")
}
#[test]
fn retained_replay_seeks_and_exact_endpoint_match_every_complete_frozen_sample() {
    for (name, data) in cases() {
        let native = crate::decoder::decode(data).unwrap();
        let frames = native.samples.len() / native.channels as usize;
        for target in [8000, 16000, 44100, 48000] {
            let expected = crate::decoder::decode_for_rate(data, target).unwrap();
            let mut d = Mp3StreamDecoder::retained(target);
            let (full, done) = drain(&mut d, data, None);
            assert!(done);
            assert_eq!(full, expected.samples);
            assert!(d.owner.is_some());
            for at in [0, 1, frames / 3, frames - 1, frames] {
                let (request, start, terminal) = d
                    .reposition(at as i64, None)
                    .expect("retained MP3 rejected a valid native position");
                assert_eq!(request, d.source_request());
                if name == "music" && !terminal {
                    assert_eq!(
                        request,
                        Some(0),
                        "uncached MP3 replay must request the original source offset zero"
                    );
                }
                let want = (at as u64 * target as u64).div_ceil(native.sample_rate as u64) as usize;
                assert_eq!(start, want as u64);
                assert_eq!(
                    terminal,
                    want == expected.samples.len() / native.channels as usize
                );
                let (got, done) = drain(&mut d, data, None);
                assert!(done);
                let reference = &expected.samples[want * native.channels as usize..];
                assert!(
                    got.len() == reference.len()
                        && got
                            .iter()
                            .zip(reference)
                            .all(|(a, b)| a.to_bits() == b.to_bits()),
                    "retained MP3 complete suffix differs from frozen grid: {name}/{target}/{at}, lengths={}/{}, first={:?}",
                    got.len(),
                    reference.len(),
                    got.iter()
                        .zip(reference)
                        .position(|(a, b)| a.to_bits() != b.to_bits())
                );
                if let Some(dir) = std::env::var_os("MIGO_MP3_CONTROL_PCM_DIR") {
                    std::fs::write(
                        std::path::Path::new(&dir)
                            .join(format!("decoder-{name}-{target}-{at}.f32le")),
                        got.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>(),
                    )
                    .unwrap();
                }
            }
        }
    }
}
#[test]
fn repeated_mp3_cutovers_preserve_global_filter_history_and_complete_tail() {
    for (name, data) in cases().into_iter().filter(|(n, _)| *n != "music") {
        let native = crate::decoder::decode(data).unwrap();
        for target in [8000, 16000, 44100, 48000] {
            let mut d = Mp3StreamDecoder::retained(target);
            drain(&mut d, data, None);
            let clock = d.clock.unwrap();
            let needed =
                (8192usize / native.channels as usize).max(clock.output_frames() as usize * 4);
            let loop_count = (needed as u128 * native.sample_rate as u128).div_ceil(
                target as u128 * (native.samples.len() / native.channels as usize) as u128,
            ) as usize
                + 2;
            let repeated = crate::resample_job::tests::reference(
                &crate::decoder::DecodedAudio {
                    samples: native.samples.repeat(loop_count),
                    sample_rate: native.sample_rate,
                    channels: native.channels,
                },
                target,
            );
            for at in [0, clock.output_frames() + 7, clock.output_frames() * 2 + 3] {
                let request = LoopRequest {
                    output_start: at,
                    enabled: false,
                    seek_target: None,
                };
                let (_, start, _) = d.reposition(0, Some(request)).unwrap();
                assert_eq!(start, at);
                let presentation = d.presentation().unwrap().unwrap();
                let end = presentation.output_end().unwrap();
                let count = ((at as u128 * native.sample_rate as u128 / target as u128) as usize
                    / (native.samples.len() / native.channels as usize))
                    + 1;
                let plan = StreamCutover::new(
                    clock,
                    at,
                    false,
                    d.owner.as_ref().unwrap().history.unwrap(),
                )
                .unwrap();
                let base = plan.input_origin() / d.owner.as_ref().unwrap().grid
                    * d.owner.as_ref().unwrap().grid;
                let all = native.samples.repeat(count);
                let finite = crate::resample_job::tests::reference(
                    &crate::decoder::DecodedAudio {
                        samples: all[base as usize * native.channels as usize..].to_vec(),
                        sample_rate: native.sample_rate,
                        channels: native.channels,
                    },
                    target,
                );
                let relative =
                    at - (base as u128 * target as u128 / native.sample_rate as u128) as u64;
                let (got, done) = drain(&mut d, data, None);
                assert!(done);
                let reference = &finite[relative as usize * native.channels as usize..];
                assert!(
                    got.len() == reference.len()
                        && got
                            .iter()
                            .zip(reference)
                            .all(|(a, b)| a.to_bits() == b.to_bits()),
                    "MP3 loop cutover differs from the complete global grid: {target}/{at}/{end}, lengths={}/{},first={:?}",
                    got.len(),
                    reference.len(),
                    got.iter()
                        .zip(reference)
                        .position(|(a, b)| a.to_bits() != b.to_bits())
                );
                dump(&got, &format!("loop-{name}-{target}-{at}-finite"));
            }
            let request = LoopRequest {
                output_start: 0,
                enabled: true,
                seek_target: Some(0),
            };
            d.reposition(0, Some(request)).unwrap();
            let (got, done) = drain(
                &mut d,
                data,
                Some((clock.output_frames() * 3) as usize * native.channels as usize),
            );
            assert!(!done);
            assert!(
                got.iter()
                    .zip(&repeated)
                    .all(|(a, b)| a.to_bits() == b.to_bits())
                    && got.len() <= repeated.len(),
                "continuous MP3 replay reset filter history: {target}, lengths={}/{}, first={:?}",
                got.len(),
                repeated.len(),
                got.iter()
                    .zip(&repeated)
                    .position(|(a, b)| a.to_bits() != b.to_bits())
            );
            dump(&got, &format!("loop-{name}-{target}-continuous"));
        }
    }
}
#[test]
fn invalid_replay_requests_keep_encoded_input_pcm_clock_and_original_owners() {
    let mut d = Mp3StreamDecoder::retained(16000);
    drain(&mut d, MUSIC, None);
    let before = |d: &Mp3StreamDecoder| {
        let o = d.owner.as_ref().unwrap();
        (
            o.buffer.as_ptr(),
            o.buffer.capacity(),
            (
                o.buffer.clone(),
                o.encoded_head,
                o.encoded_origin,
                o.scratch.clone(),
                o.head,
                o.output.clone(),
            ),
            o.output.as_ptr(),
            o.output.len(),
            o.pipeline.as_ref().unwrap() as *const Stream,
            d.clock,
            d.presentation().unwrap(),
            d.source_request(),
        )
    };
    let initial = before(&d);
    for (target, request) in [
        (-1, None),
        (480138, None),
        (
            0,
            Some(LoopRequest {
                output_start: 1,
                enabled: true,
                seek_target: Some(0),
            }),
        ),
        (
            0,
            Some(LoopRequest {
                output_start: u64::MAX,
                enabled: false,
                seek_target: None,
            }),
        ),
    ] {
        assert!(
            d.reposition(target, request).is_err(),
            "invalid MP3 replay request was accepted"
        );
        assert!(
            before(&d) == initial,
            "rejected MP3 replay changed old state or allocations"
        );
    }
}

#[tokio::test]
async fn retained_cached_capacities_reuse_the_same_allocations_and_free_only_on_the_original_cpu() {
    use crate::off_worker::OffWorker;
    use std::sync::Arc;
    let host = migo_io::scheduler::IoScheduler::new(1071);
    let peer = migo_io::scheduler::IoScheduler::new(1072);
    let slot = OffWorker::new(None::<Mp3StreamDecoder>, host.audio_cpu());
    let (slot, watches) = slot
        .with(|slot| {
            let data = include_bytes!("decoder/mp3/fixtures/tiny-47-stereo.mp3");
            let mut d = Mp3StreamDecoder::retained(16000);
            drain(&mut d, data, None);
            let pointer = d.owner.as_ref().unwrap().buffer.as_ptr();
            for _ in 0..5 {
                assert_eq!(
                    d.reposition(0, None).unwrap().0,
                    None,
                    "cached MP3 unnecessarily reopened its source"
                );
                drain(&mut d, data, None);
                assert_eq!(
                    d.owner.as_ref().unwrap().buffer.as_ptr(),
                    pointer,
                    "cached MP3 replaced its original encoded allocation"
                );
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
            "original CPU did not really free retained MP3 capacity"
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
    assert!(
        held,
        "blocked original CPU prematurely freed retained MP3 capacity"
    );
    assert!(
        watches.iter().all(|(w, n)| w.freed_bytes() == Some(*n)),
        "original CPU did not destroy every actual retained MP3 allocation"
    );
    drop(slot);
    host.close();
    peer.close();
}

fn dump(got: &[f32], key: &str) {
    if let Some(dir) = std::env::var_os("MIGO_MP3_CONTROL_PCM_DIR") {
        std::fs::write(
            std::path::Path::new(&dir).join(format!("{key}.f32le")),
            got.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>(),
        )
        .unwrap();
    }
}
