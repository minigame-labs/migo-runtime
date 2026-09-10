//! The committed clear-to-blue frame, read the way the engine reads it.
//!
//! `scripts/fixtures/external-frames/clear-blue-frame.bin` exists so that a
//! consumer needing a valid packet -- a Swift test handing one to
//! `migo_session_submit_external_frame` -- does not have to build one. Building
//! one there would be a THIRD implementation of the wire format, in a language
//! neither `contracts/frame-wire/wire-v1.md` nor the golden corpus checks, and
//! implementations that agree with each other rather than with the document are
//! the failure mode the document exists to prevent.
//!
//! That argument only holds if the committed bytes really are what they claim.
//! This reads them through the same validator the ingress uses and asserts every
//! field a consumer depends on. A fixture that had rotted would otherwise be
//! found by a pixel comparison on a device, which is the most expensive place to
//! find it -- and the failure there would say "the frame was rejected" without
//! naming the field.

use std::path::PathBuf;

use frame_wire::{stream, validate};

fn fixture() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../scripts/fixtures/external-frames/clear-blue-frame.bin");
    std::fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "read {}: {error}. Regenerate with `node \
             platforms/apple/WebContent/PerformancePlus/test/emit-clear-frame.mjs`",
            path.display()
        )
    })
}

#[test]
fn the_committed_frame_is_a_valid_packet_addressed_to_the_session_that_uses_it() {
    let bytes = fixture();
    let frame = validate(&bytes).expect("the committed fixture is a valid packet");

    // Each of these is a field the ingress checks, and each has a reason for its
    // value that the emitter states. Naming them here means a drift is reported
    // as "the nonce changed" rather than as an ingress refusal code.
    assert_eq!(frame.launch_nonce(), 0xa3, "launch nonce, matching the session config");
    assert_eq!(frame.sequence(), 1, "sequence: the first frame of the session");
    assert_eq!(frame.runtime_generation(), 1, "INITIAL_RUNTIME_GENERATION");
    // Accepted whether or not the renderer has reported the surface yet: the
    // ingress skips this check while its own generation is still 0, and matches
    // it once it is 1. Both readings accept 1, which is what makes the fixture
    // deterministic rather than a race.
    assert_eq!(frame.surface_generation(), 1, "surface generation, as attached");
    // This one is checked exactly, and nothing has advanced it.
    assert_eq!(frame.resource_epoch(), 0, "resource epoch");
}

#[test]
fn the_committed_frame_carries_a_structurally_valid_command_stream() {
    let bytes = fixture();
    let frame = validate(&bytes).expect("valid packet");
    let section = frame
        .command_stream()
        .expect("every packet carries a COMMAND_STREAM section");

    // The stream is words, and the validator wants them as words.
    assert_eq!(section.bytes.len() % 4, 0, "a command stream is whole words");
    let words: Vec<u32> = section
        .bytes
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect();
    let used = u32::try_from(words.len()).expect("the fixture is small");
    let validated =
        stream::validate_stream(&words, used).expect("the fixture's command stream validates");

    // Two records after the two-word header: CLEAR_COLOR (6 words) and CLEAR
    // (3). Asserted as a count rather than by decoding, because decoding needs a
    // GL context and the question here is only whether the bytes are the frame
    // the emitter says they are.
    assert_eq!(
        validated.words().len(),
        2 + 6 + 3,
        "magic, version, one CLEAR_COLOR record and one CLEAR record"
    );
    assert_eq!(validated.words()[0], stream::MAGIC, "stream magic");
    assert_eq!(validated.words()[1], stream::STREAM_VERSION, "stream version");

    // The colour, read back out of the record the emitter wrote. Blue, opaque --
    // and the consumer compares pixels against exactly this.
    let clear_color = &validated.words()[2..8];
    assert_eq!(stream::opcode_of(clear_color[0]), frame_wire::gl::OP_CLEAR_COLOR);
    assert_eq!(clear_color[1], 1, "canvas id");
    assert_eq!(f32::from_bits(clear_color[2]), 0.0, "red");
    assert_eq!(f32::from_bits(clear_color[3]), 0.0, "green");
    assert_eq!(f32::from_bits(clear_color[4]), 1.0, "blue");
    assert_eq!(f32::from_bits(clear_color[5]), 1.0, "alpha");

    let clear = &validated.words()[8..11];
    assert_eq!(stream::opcode_of(clear[0]), frame_wire::gl::OP_CLEAR);
    assert_eq!(clear[1], 1, "canvas id");
    assert_eq!(clear[2], 0x4000, "GL_COLOR_BUFFER_BIT");
}
