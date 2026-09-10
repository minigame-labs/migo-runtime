//! The committed frames, read the way the engine reads them.
//!
//! `platforms/apple/Tests/MigoAppleRendererTests/Fixtures/*.bin` exist so that a
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
//! found by a pixel comparison on a device -- the most expensive place to find
//! it, and one whose failure says "the frame was rejected" without naming which
//! field was wrong.

use std::path::PathBuf;

use frame_wire::{stream, validate};

/// The two committed frames, and the colour each clears to.
///
/// Two, because one frame read back as the colour it cleared to is also
/// satisfied by a readback that returns a constant, and "the pixels happened to
/// be what we expected" is the shape of green this repository keeps finding. The
/// consumer submits both in one session and asserts the pixels changed.
const FRAMES: &[(&str, u64, [f32; 4])] = &[
    ("clear-blue-frame", 1, [0.0, 0.0, 1.0, 1.0]),
    ("clear-red-frame", 2, [1.0, 0.0, 0.0, 1.0]),
];

/// The third frame, which has two colours in it.
///
/// The flat ones cannot establish that `readPixels` honours its rectangle,
/// because every rectangle of a flat surface has the same bytes. This one
/// clears the lower-left quadrant behind a scissor, so an ignored origin gives
/// an answer the consumer can tell apart.
const SCISSOR_FRAME: (&str, u64) = ("clear-scissor-frame", 3);

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../../platforms/apple/Tests/MigoAppleRendererTests/Fixtures/{name}.bin"
    ));
    std::fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "read {}: {error}. Regenerate with `node \
             platforms/apple/WebContent/PerformancePlus/test/emit-clear-frame.mjs`",
            path.display()
        )
    })
}

#[test]
fn the_committed_frames_are_packets_addressed_to_the_session_that_uses_them() {
    for (name, sequence, _) in FRAMES {
        let bytes = fixture(name);
        let frame = validate(&bytes).expect("the committed fixture is a valid packet");

        // Each is a field the ingress checks. Naming them here means a drift is
        // reported as "the nonce changed" rather than as a refusal code.
        assert_eq!(frame.launch_nonce(), 0xa3, "{name}: nonce, matching the session config");
        assert_eq!(frame.sequence(), *sequence, "{name}: sequence");
        assert_eq!(frame.runtime_generation(), 1, "{name}: INITIAL_RUNTIME_GENERATION");
        // Accepted whether or not the renderer has reported the surface yet: the
        // ingress skips this check while its own generation is still 0, and
        // matches it once it is 1. Both readings accept 1, which is what makes
        // these fixtures deterministic rather than a race.
        assert_eq!(frame.surface_generation(), 1, "{name}: surface generation, as attached");
        // This one is checked exactly, and nothing has advanced it.
        assert_eq!(frame.resource_epoch(), 0, "{name}: resource epoch");
    }
}

#[test]
fn the_committed_frames_clear_to_the_colours_their_consumer_asserts_on() {
    for (name, _, rgba) in FRAMES {
        let bytes = fixture(name);
        let frame = validate(&bytes).expect("valid packet");
        let section = frame
            .command_stream()
            .expect("every packet carries a COMMAND_STREAM section");

        assert_eq!(section.bytes.len() % 4, 0, "{name}: a command stream is whole words");
        let words: Vec<u32> = section
            .bytes
            .chunks_exact(4)
            .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect();
        let used = u32::try_from(words.len()).expect("the fixtures are small");
        let validated = stream::validate_stream(&words, used)
            .unwrap_or_else(|error| panic!("{name}: the command stream is invalid: {error:?}"));

        // Two records after the two-word header: CLEAR_COLOR (6 words) and CLEAR
        // (3). Counted rather than decoded, because decoding wants a GL context
        // and the question here is only whether these are the frames the
        // emitter says they are.
        let words = validated.words();
        assert_eq!(words.len(), 2 + 6 + 3, "{name}: header, one CLEAR_COLOR, one CLEAR");
        assert_eq!(words[0], stream::MAGIC, "{name}: stream magic");
        assert_eq!(words[1], stream::STREAM_VERSION, "{name}: stream version");

        assert_eq!(stream::opcode_of(words[2]), frame_wire::gl::OP_CLEAR_COLOR, "{name}");
        assert_eq!(words[3], 1, "{name}: canvas id");
        for (index, component) in rgba.iter().enumerate() {
            assert_eq!(
                f32::from_bits(words[4 + index]),
                *component,
                "{name}: colour component {index}"
            );
        }

        assert_eq!(stream::opcode_of(words[8]), frame_wire::gl::OP_CLEAR, "{name}");
        assert_eq!(words[9], 1, "{name}: canvas id");
        assert_eq!(words[10], 0x4000, "{name}: GL_COLOR_BUFFER_BIT");
    }
}

#[test]
fn the_committed_scissor_frame_is_the_two_colour_frame_its_consumer_reads() {
    let (name, sequence) = SCISSOR_FRAME;
    let bytes = fixture(name);
    let frame = validate(&bytes).expect("the scissored fixture is a valid packet");
    assert_eq!(frame.launch_nonce(), 0xa3, "{name}: nonce");
    assert_eq!(frame.sequence(), sequence, "{name}: sequence");
    assert_eq!(frame.surface_generation(), 1, "{name}: surface generation");

    let section = frame.command_stream().expect("a COMMAND_STREAM section");
    let words: Vec<u32> = section
        .bytes
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect();
    let used = u32::try_from(words.len()).expect("small");
    let validated = stream::validate_stream(&words, used)
        .unwrap_or_else(|error| panic!("{name}: the command stream is invalid: {error:?}"));
    let words = validated.words();

    // Header, CLEAR_COLOR, CLEAR, ENABLE, SCISSOR, CLEAR_COLOR, CLEAR, DISABLE.
    assert_eq!(words.len(), 2 + 6 + 3 + 3 + 6 + 6 + 3 + 3, "{name}: record count");

    // The order is the point: a scissor that arrived after the second clear
    // would paint the whole surface red and the consumer would still see two
    // different colours -- at the wrong places.
    let opcodes: Vec<u32> = [2usize, 8, 11, 14, 20, 26, 29]
        .iter()
        .map(|at| stream::opcode_of(words[*at]))
        .collect();
    assert_eq!(
        opcodes,
        vec![
            frame_wire::gl::OP_CLEAR_COLOR,
            frame_wire::gl::OP_CLEAR,
            frame_wire::gl::OP_ENABLE,
            frame_wire::gl::OP_SCISSOR,
            frame_wire::gl::OP_CLEAR_COLOR,
            frame_wire::gl::OP_CLEAR,
            frame_wire::gl::OP_DISABLE,
        ],
        "{name}: the records are not in the order the consumer's assertions assume"
    );

    // Blue first, then the quadrant, then red.
    assert_eq!(f32::from_bits(words[6]), 1.0, "{name}: the background is blue");
    // SCISSOR is H C I I I I: the header is at 14 and the canvas id at 15, so
    // the rectangle starts at 16. Writing 15 here was an off-by-one, and the
    // assertion caught it with "scissor x: left 1" -- which is the canvas id.
    assert_eq!(words[16], 0, "{name}: scissor x");
    assert_eq!(words[17], 0, "{name}: scissor y");
    assert_eq!(words[18], 32, "{name}: scissor width");
    assert_eq!(words[19], 32, "{name}: scissor height");
    assert_eq!(f32::from_bits(words[22]), 1.0, "{name}: the quadrant is red");
}
