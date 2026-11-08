use super::tests::wave;
use super::*;
fn configured(file: &[u8]) -> WavPacketizer {
    let mut p = WavPacketizer::new(PacketizerOptions::default());
    let mut at = 0;
    while p.track().is_none() {
        match p
            .next(&file[at..], false)
            .expect("complete WAVE seek fixture")
        {
            PacketStep::Discard(n) => at += n,
            _ => panic!("WAVE header did not finish"),
        }
    }
    p
}
#[test]
fn seek_resume_matches_original_payload_and_rejects_without_moving_cursor() {
    for channels in [1, 2] {
        for bits in [8, 16, 24, 32] {
            let (file, data) = wave(channels, bits, false, 2317, 3);
            let width = usize::from(channels * bits / 8);
            let mut p = configured(&file);
            let start = p.position();
            for target in [1731, 2316, 0] {
                let before = (p.position(), p.source_request());
                for bad in [-1, 2318] {
                    assert!(p.seek(Timestamp::new(bad)).is_err());
                    assert_eq!(
                        (p.position(), p.source_request()),
                        before,
                        "WAVE refusal changed original cursor"
                    );
                }
                let at = p
                    .seek(Timestamp::new(target))
                    .expect("complete WAVE seek fixture");
                assert_eq!(at.required_ts, Timestamp::new(target));
                let offset = p.source_request().expect("complete WAVE seek fixture");
                assert_eq!(offset, start + at.actual_ts.get() as u64 * width as u64);
                assert!(
                    matches!(p.next(&[],false).expect("complete WAVE seek fixture"),PacketStep::ReadAt(n) if n==offset)
                );
                p.resume_at(offset).expect("complete WAVE seek fixture");
                let mut pos = offset as usize;
                let mut got = Vec::new();
                loop {
                    match p
                        .next(&file[pos..], true)
                        .expect("complete WAVE seek fixture")
                    {
                        PacketStep::Packet { consumed, packet } => {
                            got.extend_from_slice(packet.data);
                            pos += consumed;
                        }
                        PacketStep::End => break,
                        _ => panic!("seeked WAVE did not emit PCM"),
                    }
                }
                assert_eq!(
                    got,
                    data[at.actual_ts.get() as usize * width..],
                    "WAVE seek dropped or changed complete payload tail"
                );
            }
        }
    }
}
#[test]
fn mismatched_resume_and_parser_errors_cannot_restart_the_representation() {
    let (file, _) = wave(2, 16, false, 2317, 0);
    let mut p = configured(&file);
    p.seek(Timestamp::ZERO).expect("complete WAVE seek fixture");
    let offset = p.source_request().expect("complete WAVE seek fixture");
    assert!(p.resume_at(offset + 4).is_err());
    assert!(
        p.resume_at(offset).is_err()
            && p.seek(Timestamp::ZERO).is_err()
            && p.next(&file, true).is_err(),
        "failed WAVE resume became reusable"
    );
    let mut p = WavPacketizer::new(PacketizerOptions::default());
    assert!(p.next(b"not-wave!!!!", true).is_err());
    assert!(p.seek(Timestamp::ZERO).is_err());
}
#[test]
fn unknown_extent_stays_unseekable_and_endpoint_keeps_packet_alignment() {
    let (file, _) = wave(1, 16, false, 17, 0);
    let mut p = configured(&file);
    let data = p.position();
    p.seek(Timestamp::new(17))
        .expect("complete WAVE seek fixture");
    let offset = p.source_request().expect("complete WAVE seek fixture");
    p.resume_at(offset).expect("complete WAVE seek fixture");
    let mut at = offset as usize;
    let mut frames = 0;
    loop {
        match p
            .next(&file[at..], true)
            .expect("complete WAVE seek fixture")
        {
            PacketStep::Packet { consumed, packet } => {
                frames += packet.dur.get();
                at += consumed
            }
            PacketStep::End => break,
            _ => panic!(),
        }
    }
    assert!(frames <= 17);
    assert!(data <= offset);
    let mut unknown = file.clone();
    unknown[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    unknown[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    let mut p = configured(&unknown);
    let before = p.position();
    assert!(p.seek(Timestamp::ZERO).is_err());
    assert_eq!(p.position(), before);
}

#[test]
fn pinned_source_extent_refuses_truncated_data_before_exposing_a_track() {
    let (file, _) = wave(2, 16, false, 2317, 3);
    let mut p = WavPacketizer::with_object_len(PacketizerOptions::default(), file.len() as u64 - 1);
    let mut at = 0;
    loop {
        match p.next(&file[at..file.len() - 1], false) {
            Ok(PacketStep::Discard(n)) => at += n,
            Err(_) => break,
            _ => panic!("truncated pinned WAVE exposed a successful packet"),
        }
    }
    assert!(
        p.track().is_none() && p.source_request().is_none(),
        "truncated pinned WAVE exposed seekable track before source bounds"
    );
    assert!(p.seek(Timestamp::ZERO).is_err() && p.next(&file, true).is_err());
}
