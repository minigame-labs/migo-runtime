#![no_main]

//! The command-stream decoder against arbitrary words.
//!
//! `frame-wire` fuzzes the envelope; this is the layer under it. A producer in another process
//! (the WebKit lane on iOS, a C host) sends records, and this decodes them into the commands the
//! renderer runs. The structural pass promises the shape of the words; the property here is that
//! nothing the decode pass reads after that promise can panic, read out of bounds, loop without
//! bound, or allocate a size the input chose -- the budget check that runs before the decode is
//! part of what is fuzzed, because it is what bounds the allocation.
//!
//! What is not asserted: that a record is accepted. A record the validators refuse is skipped,
//! and skipping is correct; this target only cares that no input gets past the guards to a fault.
//!
//! The context stages uploads and keeps transform-feedback state the way the engine's does, so
//! the staging records and the transform-feedback rules are reachable rather than refused at the
//! door.
//!
//! Run:  cargo +nightly fuzz run render_stream --fuzz-dir engine/fuzz/frame-decode
//! Corpus seeds: the packets under contracts/frame-wire/golden/ carry command streams; their
//! words, without the envelope, are the inputs a real producer sends.

use frame_decode::{
    GlDecodeContext, ImageUpload, StagedPayload, TransformFeedbackPhase, decode_render_stream,
    validate_frame_budget,
};
use frame_wire::stream::{MAGIC, STREAM_VERSION, validate_frame_stream};
use libfuzzer_sys::fuzz_target;
use shared::protocol::render_cmd::GLCmd;
use std::collections::HashSet;

/// The decoded frame the engine admits at most (the budget it gives `validate_frame_budget`).
const MAX_DECODED_BYTES: usize = 4 * 1024 * 1024;

struct Host {
    capturing: HashSet<u32>,
    staged: StagedPayload,
    errors: usize,
}

impl GlDecodeContext for Host {
    fn push_error(&mut self, _canvas_id: u32, _code: u32) {
        self.errors += 1;
    }
    fn transform_feedback_captures(&self, canvas_id: u32) -> bool {
        self.capturing.contains(&canvas_id)
    }
    fn set_transform_feedback(&mut self, canvas_id: u32, phase: TransformFeedbackPhase) {
        // Capture is active only in `Active`; paused and inactive both allow `bindBufferBase`.
        if phase == TransformFeedbackPhase::Active {
            self.capturing.insert(canvas_id);
        } else {
            self.capturing.remove(&canvas_id);
        }
    }
    fn image_upload(&mut self, _upload: ImageUpload) -> Option<GLCmd> {
        None
    }
    fn staged_payload(&mut self) -> Option<&mut StagedPayload> {
        Some(&mut self.staged)
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    let mut words: Vec<u32> = Vec::with_capacity(rest.len() / 4 + 2);
    // Bit 0 of the first byte lets a case start at the records: without it the fuzzer spends its
    // early budget rediscovering the magic number and the version.
    if mode & 1 == 1 {
        words.extend([MAGIC, STREAM_VERSION]);
    }
    words.extend(
        rest.chunks_exact(4)
            .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]])),
    );

    let Ok(stream) = validate_frame_stream(&words, words.len() as u32) else {
        return;
    };
    let Ok(_plan) = validate_frame_budget(&stream, MAX_DECODED_BYTES) else {
        return;
    };
    let mut host = Host {
        capturing: HashSet::new(),
        staged: StagedPayload::new(),
        errors: 0,
    };
    let mut ops = Vec::new();
    let decoded = decode_render_stream(&mut host, stream, &mut ops);
    std::hint::black_box((decoded, ops.len(), host.errors));
});
