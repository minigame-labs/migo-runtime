//! The decoder, exercised without a JavaScript engine anywhere in sight.
//!
//! That is the property this crate exists for, and it is asserted by the fact
//! that this test binary links and runs: `migo-frame-decode` depends on
//! `migo-shared` and `migo-frame-wire`, and
//! `scripts/test-apple-performance-rust-closure.sh` measures the resolved graph
//! of the product that uses it.
//!
//! The other half -- that it decodes the same commands the runtime's raw op
//! handlers build -- is checked by the runtime's own suite, which now calls
//! this code through a two-method adapter. Duplicating those cases here would
//! be a second set of expectations to keep in step; what is checked here is the
//! behaviour that has no other home: what the decoder does with a context, and
//! what it does with records it must reject.

use frame_decode::{GlDecodeContext, codes, decode_validated_stream};
use frame_wire::gl::{OP_BIND_BUFFER_BASE, OP_CLEAR, OP_CLEAR_COLOR, OP_SCISSOR};
use frame_wire::stream::{MAGIC, STREAM_VERSION, pack_header, validate_stream};
use shared::protocol::render_cmd::GLCmd;

/// A host with no runtime behind it: the errors go into a list.
#[derive(Default)]
struct RecordingContext {
    errors: Vec<(u32, u32)>,
    capturing: bool,
}

impl GlDecodeContext for RecordingContext {
    fn image_upload(
        &mut self,

        _upload: frame_decode::ImageUpload,
    ) -> Option<shared::protocol::render_cmd::GLCmd> {
        None
    }
    fn push_error(&mut self, canvas_id: u32, code: u32) {
        self.errors.push((canvas_id, code));
    }

    fn transform_feedback_captures(&self, _canvas_id: u32) -> bool {
        self.capturing
    }
    fn set_transform_feedback(
        &mut self,
        _canvas_id: u32,
        _phase: frame_decode::TransformFeedbackPhase,
    ) {
    }
}

/// `word_count` in a record header counts the header itself, which is what a
/// fixture written from the opcode name alone gets wrong.
fn record(opcode: u32, words: &[u32]) -> Vec<u32> {
    let mut out = vec![pack_header(opcode, words.len() as u32 + 1)];
    out.extend_from_slice(words);
    out
}

fn stream_of(records: &[Vec<u32>]) -> Vec<u32> {
    let mut words = vec![MAGIC, STREAM_VERSION];
    for record in records {
        words.extend_from_slice(record);
    }
    words
}

fn decode(words: &[u32]) -> (Vec<GLCmd>, RecordingContext) {
    let stream =
        validate_stream(words, words.len() as u32).expect("the fixture is structurally valid");
    let mut context = RecordingContext::default();
    let mut out = Vec::new();
    decode_validated_stream(&mut context, stream, &mut out);
    (out, context)
}

#[test]
fn a_clear_frame_decodes_without_a_javascript_engine() {
    let words = stream_of(&[
        record(OP_CLEAR_COLOR, &[1, 0, 0, 0, 0x3F80_0000]),
        record(OP_CLEAR, &[1, 0x4000]),
    ]);

    let (commands, context) = decode(&words);
    assert_eq!(commands.len(), 2, "both records decoded");
    assert!(context.errors.is_empty(), "a legal frame pushes no errors");
}

/// WebGL's rule for an illegal call: push an error, skip the call, keep going.
/// Not "abort the frame" and not "throw" -- a game that made one bad call still
/// draws the rest of its frame.
#[test]
fn an_illegal_call_is_skipped_and_reported_rather_than_ending_the_frame() {
    // A negative scissor height. The producer's own shim should have caught it;
    // the decoder cannot assume the producer is correct, because on iOS the
    // producer is content JavaScript in another process.
    let words = stream_of(&[
        record(OP_SCISSOR, &[1, 0, 0, 64, (-1i32) as u32]),
        record(OP_CLEAR, &[1, 0x4000]),
    ]);

    let (commands, context) = decode(&words);
    assert_eq!(
        context.errors,
        vec![(1, codes::INVALID_VALUE)],
        "the illegal viewport is reported against its canvas"
    );
    assert_eq!(
        commands.len(),
        1,
        "the bad call is skipped and the following one still decodes"
    );
}

/// The context is asked, not assumed. `bindBufferBase` on a transform feedback
/// buffer is legal or not depending on state only the host has, and a decoder
/// that guessed would either reject legal frames or accept illegal ones.
#[test]
fn the_host_decides_what_only_the_host_knows() {
    const TRANSFORM_FEEDBACK_BUFFER: u32 = 0x8C8E;

    let words = stream_of(&[record(
        OP_BIND_BUFFER_BASE,
        &[1, TRANSFORM_FEEDBACK_BUFFER, 0, 1],
    )]);

    let stream = validate_stream(&words, words.len() as u32).expect("structurally valid");
    let mut idle = RecordingContext::default();
    let mut out = Vec::new();
    decode_validated_stream(&mut idle, stream, &mut out);
    assert!(
        idle.errors.is_empty(),
        "legal while feedback is not capturing"
    );
    assert_eq!(out.len(), 1);

    let stream = validate_stream(&words, words.len() as u32).expect("structurally valid");
    let mut capturing = RecordingContext {
        capturing: true,
        ..Default::default()
    };
    let mut out = Vec::new();
    decode_validated_stream(&mut capturing, stream, &mut out);
    assert_eq!(
        capturing.errors,
        vec![(1, codes::INVALID_OPERATION)],
        "illegal while feedback is capturing"
    );
    assert!(out.is_empty(), "and the call is skipped");
}

/// The claim this crate exists for, stated as a test so a convenience
/// dependency added later cannot quietly undo it.
#[test]
fn the_decoder_is_reachable_without_a_javascript_engine() {
    // Resolving at all is the check: this binary links `migo-frame-decode`,
    // whose dependencies are `migo-shared` and `migo-frame-wire`. The product
    // closure is measured separately by
    // scripts/test-apple-performance-rust-closure.sh.
    let _ = decode_validated_stream::<RecordingContext>;
    let _ = codes::INVALID_ENUM;
}
