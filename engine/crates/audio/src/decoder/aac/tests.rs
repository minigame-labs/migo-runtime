use super::*;
use std::{io::Cursor, sync::Arc};
use symphonia_core::{
    codecs::audio::{AudioDecoder, AudioDecoderOptions},
    formats::{FormatOptions, FormatReader, TrackType},
    io::MediaSourceStream,
};

#[path = "fixtures.rs"]
mod fixtures;
fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/decoder/aac-fixtures")
            .join(name),
    )
    .unwrap()
}

// Independent mature pull reader: no product packetizer, source resolver,
// collector, work loop or format dispatcher participates in this oracle.
pub(crate) fn reference(data: &[u8], m4a: bool) -> DecodedAudio {
    let source = MediaSourceStream::new(Box::new(Cursor::new(data)), Default::default());
    let mut reader: Box<dyn FormatReader> = if m4a {
        Box::new(
            symphonia_format_isomp4::IsoMp4Reader::try_new(source, FormatOptions::default())
                .unwrap(),
        )
    } else {
        Box::new(
            symphonia_codec_aac::AdtsReader::try_new(source, FormatOptions::default()).unwrap(),
        )
    };
    let track = reader.default_track(TrackType::Audio).unwrap();
    let id = track.id;
    let params = track.codec_params.as_ref().unwrap().audio().unwrap();
    let mut codec = symphonia_codec_aac::AacDecoder::try_new(
        params,
        &AudioDecoderOptions::default().gapless(true),
    )
    .unwrap();
    let mut result = DecodedAudio {
        samples: Vec::new(),
        sample_rate: 0,
        channels: 0,
    };
    while let Some(packet) = reader.next_packet().unwrap() {
        if packet.track_id != id {
            continue;
        }
        let block = codec.decode(&packet).unwrap();
        result.sample_rate = block.spec().rate();
        result.channels = block.spec().channels().count() as u32;
        let start = result.samples.len();
        result
            .samples
            .resize(start + block.samples_interleaved(), 0.);
        block.copy_to_slice_interleaved(&mut result.samples[start..]);
    }
    result
}
fn drain<B: AsRef<[u8]>>(mut job: AacDecodeJob<B>) -> DecodedAudio {
    for _ in 0..MAX_STEPS {
        if job.step(&AtomicBool::new(false)).unwrap_or_else(|_| {
            panic!("AAC product job failed its complete PCM or ownership contract")
        }) {
            return job.finish().unwrap();
        }
    }
    panic!("AAC product job did not finish");
}

#[test]
fn incomplete_adts_frames_and_movie_indexes_never_publish_partial_pcm() {
    for (name, m4a) in [("adts-1.aac", false), ("1-tail.m4a", true)] {
        let bytes = fixture(name);
        for length in [4, 7, bytes.len() / 2, bytes.len() - 1] {
            let mut job = AacDecodeJob::new(bytes[..length].to_vec(), None, m4a).unwrap();
            let mut failed = false;
            for _ in 0..4096 {
                match job.step(&AtomicBool::new(false)) {
                    Err(e) => {
                        assert_eq!(e.code, ErrorCode::InvalidArgument);
                        failed = true;
                        break;
                    }
                    Ok(done) => assert!(!done, "AAC truncated source published partial PCM"),
                }
            }
            assert!(
                failed,
                "AAC truncated source did not fail within bounded work"
            );
        }
    }
}
#[test]
fn every_product_sample_and_submitted_rate_matches_independent_pull_and_src() {
    let mut samples = 0;
    for &(name, data, rate, channels, frames) in fixtures::CASES {
        let m4a = name.ends_with(".m4a");
        let expected = reference(data, m4a);
        assert_eq!(
            (
                expected.sample_rate,
                expected.channels,
                expected.frame_count()
            ),
            (rate, channels, frames),
            "independent presentation: {name}"
        );
        let native = drain(AacDecodeJob::new(data, None, m4a).unwrap());
        assert_eq!(
            native.samples, expected.samples,
            "AAC complete native PCM changed: {name}"
        );
        for (variant, target) in [rate, 16000, 48000].into_iter().enumerate() {
            let expected = crate::resampler::resample_if_needed(expected.clone(), target).unwrap();
            let actual = crate::decoder::decode_for_rate(data, target).unwrap();
            assert_eq!(
                (actual.sample_rate, actual.channels),
                (target, channels),
                "AAC submitted target changed"
            );
            assert_eq!(
                actual.samples, expected.samples,
                "AAC complete converted PCM changed: {name}/{target}"
            );
            samples += actual.samples.len();
            if std::env::var_os("MIGO_DUMP_AAC_PCM").is_some() {
                println!(
                    "MIGO_AAC_PCM {name}/{variant}/{target} {target} {channels} {:?}",
                    actual
                        .samples
                        .iter()
                        .map(|x| x.to_bits())
                        .collect::<Vec<_>>()
                );
            }
        }
    }
    println!(
        "AAC_COMPLETE_COMPARISON {} {samples}",
        fixtures::CASES.len()
    );
}

#[test]
fn adts_matches_the_entire_independent_ffmpeg_pcm() {
    for number in 0..3 {
        let bytes = fixture(&format!("adts-{number}.aac"));
        let result = drain(AacDecodeJob::new(bytes, None, false).unwrap());
        let pcm = fixture(&format!("adts-{number}.f32le"));
        let expected: Vec<_> = pcm
            .chunks_exact(4)
            .map(|x| f32::from_le_bytes(x.try_into().unwrap()))
            .collect();
        assert_eq!(
            result.samples.len(),
            expected.len(),
            "ADTS independent full duration changed"
        );
        let mut square = 0.;
        let mut maximum = 0_f64;
        for (actual, expected) in result.samples.iter().zip(&expected) {
            let error = f64::from(actual - expected).abs();
            square += error * error;
            maximum = maximum.max(error);
        }
        assert!(
            maximum < 0.01 && (square / expected.len() as f64).sqrt() < 0.001,
            "ADTS independent full PCM differs"
        );
    }
}

#[test]
fn tagged_adts_uses_library_tag_extents_and_never_confuses_mp3_payloads() {
    fn tagged(payload: &[u8], body: usize) -> Vec<u8> {
        let mut bytes = b"ID3\x04\0\0".to_vec();
        bytes.extend([
            (body >> 21) as u8 & 127,
            (body >> 14) as u8 & 127,
            (body >> 7) as u8 & 127,
            body as u8 & 127,
        ]);
        bytes.resize(10 + body, 0);
        bytes[10..14].copy_from_slice(b"\xff\xf1\x50\x80");
        bytes.extend(payload);
        bytes
    }
    let original = fixture("adts-1.aac");
    let expected = crate::decoder::decode_for_rate(&original, 48000).unwrap();
    for body in [128, 65536, 3 * 1024 * 1024] {
        let bytes = tagged(&original, body);
        let actual = crate::decoder::decode_for_rate(&bytes, 48000)
            .unwrap_or_else(|_| panic!("tagged ADTS was misrouted or metadata became PCM"));
        assert_eq!(
            actual.samples, expected.samples,
            "tagged ADTS was misrouted or metadata became PCM"
        );
    }
    let mp3 = crate::mp3_fixture::stream(4);
    let expected = crate::decoder::decode_for_rate(&mp3, 48000).unwrap();
    assert_eq!(
        crate::decoder::decode_for_rate(&tagged(&mp3, 65536), 48000)
            .unwrap()
            .samples,
        expected.samples,
        "ID3 body sync changed MP3 dispatch"
    );
    for mut bad in [tagged(&original, 128), tagged(&original, 65536)] {
        bad[6] = 0x7f;
        assert!(
            crate::decoder::decode_for_rate(&bad, 48000).is_err(),
            "invalid ID3 extent was accepted"
        );
    }
}

#[test]
fn each_input_and_pcm_copy_remains_bounded_across_long_indexed_sources() {
    for (file, m4a) in [("2-tail-reverse.m4a", true), ("adts-1.aac", false)] {
        let mut job = AacDecodeJob::new(fixture(file), None, m4a).unwrap();
        let mut copies = 0;
        for _ in 0..MAX_STEPS {
            let before = if let Phase::Decode(d) = &job.phase {
                Some((
                    stage(&job),
                    d.offset,
                    d.head,
                    d.pcm.len(),
                    d.collector.needs_reservation(),
                ))
            } else {
                None
            };
            let done = job
                .step(&AtomicBool::new(false))
                .unwrap_or_else(|_| panic!("AAC step exceeded its bounded operation contract"));
            if let (Some((old, offset, head, pcm, reserving)), Phase::Decode(d)) =
                (before, &job.phase)
            {
                if head < pcm && !reserving {
                    copies += 1;
                    assert!(
                        d.head >= head && d.head - head <= 8192,
                        "AAC PCM copy exceeded its step"
                    );
                } else if old == 0 {
                    assert!(
                        d.offset >= offset && d.offset - offset <= 8192,
                        "AAC encoded input exceeded its step"
                    );
                }
            }
            if done {
                break;
            }
        }
        assert!(copies > 1);
        assert!(!job.finish().unwrap().samples.is_empty());
    }
}

fn stage<B>(job: &AacDecodeJob<B>) -> usize {
    match &job.phase {
        Phase::Decode(d) => match &d.codec {
            Codec::M4a(f) => f.source_stage(),
            Codec::Adts(_) => 0,
        },
        Phase::Flatten(..) => 4,
        Phase::Decoded(_) => 5,
        Phase::Resample(_) => 6,
        Phase::Complete(_) => 7,
        Phase::Failed(_) => 8,
    }
}
#[test]
fn cancellation_in_each_codec_source_collection_and_src_phase_releases_actual_input() {
    for m4a in [false, true] {
        for wanted in if m4a {
            vec![0, 1, 2, 3, 4, 5, 6]
        } else {
            vec![0, 4, 5, 6]
        } {
            let data = fixture(if m4a {
                "2-tail-reverse.m4a"
            } else {
                "adts-1.aac"
            });
            let capacity = data.capacity();
            let watch = migo_alloc_probe::DeallocationWatch::new(data.as_ptr()).unwrap();
            let data = Arc::new(data);
            let weak = Arc::downgrade(&data);
            struct Input(Arc<Vec<u8>>);
            impl AsRef<[u8]> for Input {
                fn as_ref(&self) -> &[u8] {
                    &self.0
                }
            }
            let mut job = AacDecodeJob::new(Input(data), Some(16000), m4a).unwrap();
            for _ in 0..MAX_STEPS {
                if stage(&job) == wanted {
                    break;
                }
                assert!(!job.step(&AtomicBool::new(false)).unwrap());
            }
            assert_eq!(stage(&job), wanted, "AAC cancellation phase not reached");
            if wanted < 4 {
                assert!(weak.upgrade().is_some());
                assert!(watch.freed_bytes().is_none());
            }
            assert!(
                matches!(job.step(&AtomicBool::new(true)), Err(e) if e.code == ErrorCode::Cancelled),
                "AAC cancellation was ignored"
            );
            assert!(
                weak.upgrade().is_none(),
                "AAC cancellation retained the original input"
            );
            assert_eq!(
                watch.freed_bytes(),
                Some(capacity),
                "AAC encoded allocation was not actually freed"
            );
            assert_eq!(
                job.step(&AtomicBool::new(false)).unwrap_err().code,
                ErrorCode::Cancelled,
                "AAC terminal error changed"
            );
            assert_eq!(job.finish().unwrap_err().code, ErrorCode::Cancelled);
        }
    }
}

#[test]
fn index_atoms_yield_without_copying_bodies_and_preserve_codec_history() {
    let data = fixture("valid-many.m4a");
    let expected = reference(&data, true);
    let mut job = AacDecodeJob::new(data, None, true).unwrap();
    let mut scans = 0;
    for _ in 0..MAX_STEPS {
        let old = stage(&job);
        let before = migo_alloc_probe::thread_counts();
        assert!(!job.step(&AtomicBool::new(false)).unwrap());
        if old == 1 {
            scans += 1;
            let after = migo_alloc_probe::thread_counts();
            assert_eq!(
                after.allocations, before.allocations,
                "M4A atom scan allocated a body"
            );
            assert_eq!(after.reallocations, before.reallocations);
        }
        if stage(&job) == 2 {
            break;
        }
    }
    assert_eq!(scans, 25, "M4A source scan stopped yielding at each atom");
    assert_eq!(
        drain(job).samples,
        expected.samples,
        "M4A indexed resume reset codec history"
    );
}

fn atom(tag: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = ((8 + body.len()) as u32).to_be_bytes().to_vec();
    out.extend(tag);
    out.extend(body);
    out
}
#[test]
fn malformed_sources_and_profiles_fail_closed_with_bounded_work() {
    let tail = fixture("2-tail.m4a");
    let movie = tail.windows(4).position(|w| w == b"moov").unwrap() - 4;
    let mut cases = Vec::new();
    for (case, addition) in [
        atom(b"free", &[]).repeat(40),
        atom(b"free", &vec![0; 4 * 1024 * 1024]),
        atom(b"moov", &vec![0; 2 * 1024 * 1024]),
        [1_000_000_u32.to_be_bytes().as_slice(), b"free"].concat(),
        atom(b"moof", &[]),
    ]
    .into_iter()
    .enumerate()
    {
        cases.push((
            [&tail[..movie], &addition, &tail[movie..]].concat(),
            case == 2,
            case == 1,
        ));
    }
    cases.push((tail[..movie].to_vec(), false, false));
    for (data, oversized_index, oversized_metadata) in cases {
        let mut job = AacDecodeJob::new(data, None, true).unwrap();
        let mut failed = false;
        for _ in 0..512 {
            let before = stage(&job);
            match job.step(&AtomicBool::new(false)) {
                Err(e) => {
                    if oversized_metadata {
                        assert_eq!(
                            before, 1,
                            "AAC metadata budget failed after the source scan"
                        );
                    }
                    if oversized_index {
                        assert_eq!(before, 1, "AAC oversized index reached its parse phase");
                    }
                    assert_eq!(
                        e.code,
                        ErrorCode::InvalidArgument,
                        "AAC source refusal code changed"
                    );
                    assert_eq!(job.step(&AtomicBool::new(false)).unwrap_err().code, e.code);
                    assert!(job.finish().is_err());
                    failed = true;
                    break;
                }
                Ok(done) => assert!(!done, "AAC malformed source published PCM"),
            }
        }
        assert!(failed, "AAC malformed source escaped its bounded work");
    }
    let mut adts = fixture("adts-1.aac");
    adts[2] &= 0x3f;
    assert!(
        crate::decoder::decode_for_rate(&adts, 44100).is_err(),
        "unsupported AAC Main profile was accepted"
    );
}

#[test]
fn declared_native_and_converted_quotas_and_work_errors_are_terminal() {
    for m4a in [false, true] {
        let data = fixture(if m4a { "1-tail.m4a" } else { "adts-1.aac" });
        let count = reference(&data, m4a).samples.len();
        assert_eq!(
            drain(AacDecodeJob::with_limit(data.clone(), None, m4a, count).unwrap())
                .samples
                .len(),
            count
        );
        for limit in [0, count - 1] {
            let mut job = AacDecodeJob::with_limit(data.clone(), None, m4a, limit).unwrap();
            let mut failed = false;
            for _ in 0..4096 {
                match job.step(&AtomicBool::new(false)) {
                    Err(e) => {
                        assert_eq!(e.code, ErrorCode::InvalidArgument);
                        assert_eq!(job.step(&AtomicBool::new(false)).unwrap_err().code, e.code);
                        failed = true;
                        break;
                    }
                    Ok(done) => assert!(!done, "AAC output quota published partial PCM"),
                }
            }
            assert!(failed, "AAC output quota ignored");
        }
        let mut job = AacDecodeJob::new(data, Some(48000), m4a).unwrap();
        job.steps = MAX_STEPS;
        assert!(
            matches!(job.step(&AtomicBool::new(false)), Err(e) if e.code==ErrorCode::InvalidArgument),
            "AAC work budget ignored"
        );
        assert_eq!(
            job.step(&AtomicBool::new(false)).unwrap_err().code,
            ErrorCode::InvalidArgument,
            "AAC work error did not stay terminal"
        );
    }
    let mut job =
        AacDecodeJob::with_limit(fixture("1-tail.m4a"), Some(48000), true, 19474).unwrap();
    let mut failed = false;
    for _ in 0..4096 {
        match job.step(&AtomicBool::new(false)) {
            Err(e) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                failed = true;
                break;
            }
            Ok(done) => assert!(!done, "AAC converted quota ignored"),
        }
    }
    assert!(failed);
}

#[test]
fn incomplete_finish_never_publishes_and_input_retires_before_collection_or_src() {
    for m4a in [false, true] {
        let bytes = fixture(if m4a {
            "2-tail-reverse.m4a"
        } else {
            "adts-1.aac"
        });
        assert!(
            matches!(AacDecodeJob::new(bytes.clone(),None,m4a).unwrap().finish(),Err(e) if e.code==ErrorCode::Internal),
            "AAC incomplete finish published PCM"
        );
        let watch = migo_alloc_probe::DeallocationWatch::new(bytes.as_ptr()).unwrap();
        let capacity = bytes.capacity();
        let mut job = AacDecodeJob::new(bytes, Some(16000), m4a).unwrap();
        for _ in 0..MAX_STEPS {
            if stage(&job) == 4 {
                break;
            }
            assert!(!job.step(&AtomicBool::new(false)).unwrap());
        }
        assert_eq!(stage(&job), 4);
        assert_eq!(
            watch.freed_bytes(),
            Some(capacity),
            "AAC encoded input survived final collection"
        );
        assert!(!drain(job).samples.is_empty());
    }
}
