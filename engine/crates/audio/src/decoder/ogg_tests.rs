use super::*;
use shared::error::ErrorCode;
fn fixture(name: &str) -> Vec<u8> {
    let bytes: &[u8] = match name {
        "0" => include_bytes!("ogg-fixtures/0.ogg"),
        "1" => include_bytes!("ogg-fixtures/1.ogg"),
        "2" => include_bytes!("ogg-fixtures/2.ogg"),
        "cross" => include_bytes!("ogg-fixtures/cross.ogg"),
        "max-window-8" => include_bytes!("ogg-fixtures/max-window-8.ogg"),
        _ => unreachable!(),
    };
    bytes.to_vec()
}
#[test]
fn product_vorbis_exact_short_tail_and_cross_page_counts() {
    for (name, frames, rate, channels) in [
        ("0", 17, 44100, 1),
        ("1", 9737, 48000, 2),
        ("2", 9737, 22050, 1),
        ("cross", 240137, 48000, 6),
        ("max-window-8", 8192, 48000, 8),
    ] {
        let audio = super::super::decode(&fixture(name)).unwrap();
        assert_eq!(
            (audio.frame_count(), audio.sample_rate, audio.channels),
            (frames, rate, channels),
            "{name}"
        );
        assert!(audio.samples.iter().all(|sample| sample.is_finite()));
        if name == "max-window-8" {
            assert!(audio.samples.iter().all(|sample| *sample == 0.0));
        } else {
            assert!(audio.samples.iter().any(|sample| sample.abs() > 0.001));
        }
    }
}
#[test]
fn product_vorbis_full_pcm_matches_frozen_mature_decoder() {
    for (name, expected) in [
        ("0", include_bytes!("ogg-fixtures/0.f32le").as_slice()),
        ("1", include_bytes!("ogg-fixtures/1.f32le").as_slice()),
        ("2", include_bytes!("ogg-fixtures/2.f32le").as_slice()),
    ] {
        let audio = decode(&fixture(name)).unwrap();
        assert_eq!(audio.samples.len() * 4, expected.len());
        for (index, (sample, bytes)) in audio
            .samples
            .iter()
            .zip(expected.chunks_exact(4))
            .enumerate()
        {
            let reference = f32::from_le_bytes(bytes.try_into().unwrap());
            assert!(
                (*sample - reference).abs() < 1e-6,
                "{name} sample {index}: {sample} != {reference}"
            );
        }
    }
}
#[test]
fn product_vorbis_trimmed_timeline_reaches_the_submitted_rate() {
    for name in ["0", "1", "2"] {
        let data = fixture(name);
        let native = decode(&data).unwrap();
        for target in [44100, 48000] {
            let actual = super::super::decode_for_rate(&data, target).unwrap();
            assert_eq!(
                (actual.sample_rate, actual.channels),
                (target, native.channels)
            );
            assert_eq!(
                actual.frame_count(),
                (native.frame_count() * target as usize).div_ceil(native.sample_rate as usize)
            );
            let expected = crate::resampler::resample_if_needed(native.clone(), target).unwrap();
            assert_eq!(actual.samples, expected.samples);
        }
    }
}
#[test]
fn product_vorbis_caller_quota_and_corruption_never_return_partial_pcm() {
    let data = fixture("1");
    let exact = 9737 * 2;
    assert_eq!(
        decode_with_limit(&data, exact).unwrap().samples.len(),
        exact
    );
    assert!(matches!(decode_with_limit(&data,exact-1),Err(e)if e.code==ErrorCode::InvalidArgument));
    let mut broken = data.clone();
    broken.pop();
    let mut crc = data.clone();
    let n = crc.len() - 1;
    crc[n] ^= 1;
    let mut chained = data.clone();
    chained.extend_from_slice(&data);
    let oversized = decode(&vec![0; 16 * 1024 * 1024 + 1]).unwrap_err();
    assert!(
        oversized
            .detail
            .as_deref()
            .unwrap()
            .contains("encoded input exceeds")
    );
    for bad in [Vec::new(), broken, crc, chained] {
        assert!(matches!(decode(&bad),Err(e)if e.code==ErrorCode::InvalidArgument));
    }
}
