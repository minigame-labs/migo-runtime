//! The two downlink readers against each other, in both directions.
//!
//! The host writes this format and the producer reads it, so the direction that
//! matters in production is Rust-writes/JavaScript-reads. Both are checked
//! anyway, because a pair of implementations that agree only in the direction
//! somebody remembered to test is the same as one implementation.
//!
//! `#[ignore]` for the reason `js_interop.rs` gives: these need a directory
//! `cargo test` has no way to produce, and a test that returns early on a
//! missing environment variable is the silent-green shape this repository keeps
//! finding. `scripts/test-frame-wire-js-encoder.sh` is what guarantees they run.

use std::{fs, path::PathBuf};

use frame_wire::downlink::{
    DOWN_CLOCK_TICK, DOWN_FRAME_VERDICT, DownlinkRecord, decode_bytes, encode_bytes,
};

/// The spread both sides are driven with.
///
/// Values past 2^32 in every 64-bit field, because the two-word split is the
/// mistake a hand-written reader makes and a small number would not notice it.
/// This is the same reasoning the packet corpus uses for its 128-bit nonce.
fn corpus() -> Vec<Vec<DownlinkRecord>> {
    let verdict = |seq: u64, decision: u32| DownlinkRecord::FrameVerdict {
        generation: 0x1234_5678,
        decision,
        wire_error_code: if decision == 3 { 0x0000_2001 } else { 0 },
        remaining_credits: 3,
        accepted_sequence: seq,
    };
    let tick = |ns: u64, id: u32| DownlinkRecord::ClockTick {
        generation: 0x1234_5678,
        frame_id: id,
        timestamp_ns: ns,
    };
    vec![
        // An envelope with nothing in it is legal and has to survive: a host
        // with no news still answers, and a reader that mishandles the empty
        // case fails only under load.
        vec![],
        vec![verdict(1, 1)],
        vec![tick(16_666_667, 1)],
        // Both kinds in one message, which is the whole reason the envelope
        // holds a run rather than a record.
        vec![
            tick(0x0000_00FF_FFFF_FFFF, 2),
            verdict(0x0000_0100_0000_0001, 1),
        ],
        vec![verdict(0x0007_FFFF_FFFF_FFFF, 3)],
        // A long batch: the reader has to walk record lengths rather than
        // assume a count.
        (0..64)
            .map(|i| {
                if i % 2 == 0 {
                    verdict(u64::from(i) + 0x1_0000_0000, 1)
                } else {
                    tick(u64::from(i) * 16_666_667 + 0x2_0000_0000, i)
                }
            })
            .collect(),
    ]
}

fn directory(variable: &str) -> PathBuf {
    PathBuf::from(
        std::env::var(variable)
            .unwrap_or_else(|_| panic!("{variable} must name the interop directory")),
    )
}

#[test]
#[ignore = "needs a directory prepared by the gate; run through scripts/test-frame-wire-js-encoder.sh"]
fn the_rust_writer_produces_messages_for_the_javascript_reader() {
    let out = directory("MIGO_DOWNLINK_OUT_DIR");
    fs::create_dir_all(&out).expect("the output directory must be creatable");
    let mut written = 0;
    for (index, records) in corpus().into_iter().enumerate() {
        let bytes = encode_bytes(&records);
        // Round-tripped here too, so a corpus entry this crate cannot read back
        // fails on the side that wrote it rather than in another language.
        let read = decode_bytes(&bytes).expect("this crate must read what it wrote");
        assert_eq!(
            read, records,
            "entry {index} did not survive its own round trip"
        );
        fs::write(out.join(format!("downlink-{index:03}.bin")), &bytes)
            .expect("writing a corpus entry");
        written += 1;
    }
    // Printed, not inferred: a run that wrote nothing and passed is the shape
    // this gate is here to make impossible.
    println!("wrote {written} downlink messages for the JavaScript reader");
}

#[test]
#[ignore = "needs messages emitted by node; run through scripts/test-frame-wire-js-encoder.sh"]
fn messages_from_the_javascript_writer_are_read_unchanged() {
    let dir = directory("MIGO_DOWNLINK_IN_DIR");
    let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("the emitter's output directory must exist")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "bin"))
        .collect();
    entries.sort();
    assert!(
        !entries.is_empty(),
        "no messages in {}; the emitter did not run",
        dir.display()
    );

    let expected = corpus();
    assert_eq!(
        entries.len(),
        expected.len(),
        "the emitter wrote {} messages and this corpus has {}; the two sides are \
         driven by the same list and must stay the same length",
        entries.len(),
        expected.len()
    );

    let mut kinds_seen = (false, false);
    for (path, records) in entries.iter().zip(expected) {
        let bytes = fs::read(path).expect("reading an emitted message");
        let read = decode_bytes(&bytes)
            .unwrap_or_else(|error| panic!("{} was rejected: {error:?}", path.display()));
        assert_eq!(
            read,
            records,
            "{} decoded to different records",
            path.display()
        );
        for record in &read {
            match record {
                DownlinkRecord::FrameVerdict { .. } => kinds_seen.0 = true,
                DownlinkRecord::ClockTick { .. } => kinds_seen.1 = true,
            }
        }
    }
    // Both kinds, or the corpus has quietly stopped covering one of them --
    // which is how a format grows a field nothing checks.
    assert!(
        kinds_seen.0 && kinds_seen.1,
        "the corpus must exercise both {DOWN_FRAME_VERDICT} and {DOWN_CLOCK_TICK}"
    );
    println!(
        "read {} JavaScript-encoded downlink messages",
        entries.len()
    );
}
