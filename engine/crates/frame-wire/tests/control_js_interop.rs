//! The two control-message implementations against each other, in both
//! directions.
//!
//! The producer writes this format and the host reads it, so the direction
//! that matters in production is JavaScript-writes/Rust-reads. Both run anyway,
//! for the reason `downlink_js_interop.rs` gives: two implementations that agree
//! only in the direction somebody remembered to test are one implementation.
//!
//! `#[ignore]`d because they need a directory `cargo test` cannot produce;
//! `scripts/test-frame-wire-js-encoder.sh` is what guarantees they run.

use std::{fs, path::PathBuf};

use frame_wire::control::{
    CONTROL_ENVELOPE_WORDS, ControlRecord, MAX_CONTROL_WORDS, REQUEST_FRAME_WORDS, encode_control,
    read_control,
};

/// The spread both sides are driven with. `emit-control.mjs` holds the same
/// list, and each side asserts the other's length.
fn corpus() -> Vec<Vec<ControlRecord>> {
    let request = |generation: u32| ControlRecord::RequestFrame { generation };
    let longest = (MAX_CONTROL_WORDS - CONTROL_ENVELOPE_WORDS) / REQUEST_FRAME_WORDS as usize;
    vec![
        vec![request(1)],
        // Every bit of the word, because the producer builds it from a BigInt
        // and the low-32-bits step is where that goes wrong.
        vec![request(u32::MAX)],
        vec![request(0x8000_0001)],
        vec![request(1), request(2)],
        // Exactly the ceiling.
        (0..longest as u32)
            .map(|i| request(i.wrapping_mul(0x9E37_79B9)))
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
fn the_rust_writer_produces_control_messages_for_the_javascript_reader() {
    let out = directory("MIGO_CONTROL_OUT_DIR");
    fs::create_dir_all(&out).expect("the output directory must be creatable");
    let mut written = 0;
    for (index, records) in corpus().into_iter().enumerate() {
        let bytes = encode_control(&records);
        let read: Vec<_> = read_control(&bytes)
            .expect("this crate must read what it wrote")
            .records()
            .collect();
        assert_eq!(
            read, records,
            "entry {index} did not survive its own round trip"
        );
        fs::write(out.join(format!("control-{index:03}.bin")), &bytes)
            .expect("writing a corpus entry");
        written += 1;
    }
    println!("wrote {written} control messages for the JavaScript reader");
}

#[test]
#[ignore = "needs messages emitted by node; run through scripts/test-frame-wire-js-encoder.sh"]
fn control_messages_from_the_javascript_writer_are_read_unchanged() {
    let dir = directory("MIGO_CONTROL_IN_DIR");
    let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("the emitter's output directory must exist")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "bin"))
        .collect();
    entries.sort();
    let expected = corpus();
    assert_eq!(
        entries.len(),
        expected.len(),
        "the emitter wrote {} messages and this corpus has {}",
        entries.len(),
        expected.len()
    );
    for (path, records) in entries.iter().zip(expected) {
        let bytes = fs::read(path).expect("reading an emitted message");
        // Byte for byte, not only record for record: the host's reader accepting
        // a message is weaker than the producer writing the message the
        // contract describes.
        assert_eq!(
            bytes,
            encode_control(&records),
            "{} is not the bytes the reference writer produces",
            path.display()
        );
        let read: Vec<_> = read_control(&bytes)
            .unwrap_or_else(|error| panic!("{} was refused: {error:?}", path.display()))
            .records()
            .collect();
        assert_eq!(
            read,
            records,
            "{} decoded to different records",
            path.display()
        );
    }
    println!("read {} JavaScript-encoded control messages", entries.len());
}
