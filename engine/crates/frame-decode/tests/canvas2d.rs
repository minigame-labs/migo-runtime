//! The mixed 2D/GL stream: what comes out, and in what order.
//!
//! Order is the whole point of one stream carrying both. A frame draws its
//! background with 2D, its sprites with GL, and its HUD with 2D again; the
//! renderer has to see those in the order they were issued, and it has to see
//! the 2D work materialized before the GL work that draws over it.

use frame_decode::{GlDecodeContext, codes, decode_render_stream};
use frame_wire::canvas2d::*;
use frame_wire::gl::OP_CLEAR;
use frame_wire::stream::{MAGIC, STREAM_VERSION, pack_header, validate_stream};
use shared::protocol::FrameOp;
use shared::protocol::render_cmd::Canvas2DCmd;

#[derive(Default)]
struct RecordingContext {
    errors: Vec<(u32, u32)>,
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
        false
    }
    fn set_transform_feedback(
        &mut self,
        _canvas_id: u32,
        _phase: frame_decode::TransformFeedbackPhase,
    ) {
    }
}

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

fn decode(words: &[u32]) -> (Vec<FrameOp>, RecordingContext) {
    let stream = validate_stream(words, words.len() as u32).expect("structurally valid");
    let mut context = RecordingContext::default();
    let mut out = Vec::new();
    decode_render_stream(&mut context, stream, &mut out);
    (out, context)
}

fn shape(ops: &[FrameOp]) -> Vec<String> {
    ops.iter()
        .map(|op| match op {
            FrameOp::BeginFrame => "BeginFrame".to_string(),
            FrameOp::CanvasBatch(payload) => {
                format!(
                    "CanvasBatch({}, {})",
                    payload.canvas_id,
                    payload.commands.len()
                )
            }
            FrameOp::GlBatch(payload) => format!("GlBatch({})", payload.commands.len()),
            FrameOp::Materialize { canvas_id } => format!("Materialize({canvas_id})"),
            FrameOp::Present => "Present".to_string(),
        })
        .collect()
}

#[test]
fn a_pure_2d_frame_becomes_one_batch_and_a_barrier() {
    let words = stream_of(&[
        record(OP2D_SELECT_CANVAS, &[7]),
        record(OP2D_BEGIN_PATH, &[]),
        record(OP2D_MOVE_TO, &[10f32.to_bits(), 20f32.to_bits()]),
        record(OP2D_LINE_TO, &[30f32.to_bits(), 40f32.to_bits()]),
        record(OP2D_FILL, &[]),
    ]);

    let (ops, context) = decode(&words);
    assert!(context.errors.is_empty());
    assert_eq!(shape(&ops), vec!["CanvasBatch(7, 4)", "Materialize(7)"]);

    let FrameOp::CanvasBatch(batch) = &ops[0] else {
        panic!("expected a canvas batch");
    };
    assert!(matches!(batch.commands[0], Canvas2DCmd::BeginPath));
    assert!(matches!(
        batch.commands[1],
        Canvas2DCmd::MoveTo { x, y } if x == 10.0 && y == 20.0
    ));
    assert!(
        !batch.present,
        "presentation is the packet's decision, not a batch's"
    );
}

/// The barrier, which is the reason one stream is worth more than two.
#[test]
fn gl_after_2d_waits_for_the_2d_work_to_be_materialized() {
    let words = stream_of(&[
        record(OP2D_SELECT_CANVAS, &[3]),
        record(OP2D_FILL_RECT, &[0, 0, 64f32.to_bits(), 64f32.to_bits()]),
        record(OP_CLEAR, &[3, 0x4000]),
        record(OP2D_SELECT_CANVAS, &[3]),
        record(OP2D_STROKE, &[]),
    ]);

    let (ops, context) = decode(&words);
    assert!(context.errors.is_empty());
    assert_eq!(
        shape(&ops),
        vec![
            "CanvasBatch(3, 1)",
            "Materialize(3)",
            "GlBatch(1)",
            "CanvasBatch(3, 1)",
            "Materialize(3)",
        ],
        "the GL batch must not sit before the materialize of the 2D work it draws over"
    );
}

#[test]
fn switching_canvas_ends_the_batch() {
    let words = stream_of(&[
        record(OP2D_SELECT_CANVAS, &[1]),
        record(OP2D_SAVE, &[]),
        record(OP2D_SELECT_CANVAS, &[2]),
        record(OP2D_RESTORE, &[]),
    ]);

    let (ops, _) = decode(&words);
    assert_eq!(
        shape(&ops),
        vec![
            "CanvasBatch(1, 1)",
            "CanvasBatch(2, 1)",
            "Materialize(1)",
            "Materialize(2)"
        ],
        "one batch carries one canvas id"
    );
}

/// A producer that never said where to draw is reported, not guessed at.
///
/// Defaulting to canvas zero is how content ends up painting over something
/// else, and the symptom is a rendering bug with no error anywhere.
#[test]
fn a_2d_record_before_a_canvas_is_selected_is_refused() {
    let words = stream_of(&[
        record(OP2D_FILL_RECT, &[0, 0, 8f32.to_bits(), 8f32.to_bits()]),
        record(OP2D_SELECT_CANVAS, &[5]),
        record(OP2D_FILL, &[]),
    ]);

    let (ops, context) = decode(&words);
    assert_eq!(context.errors, vec![(0, codes::INVALID_OPERATION)]);
    assert_eq!(
        shape(&ops),
        vec!["CanvasBatch(5, 1)", "Materialize(5)"],
        "the orphan record is skipped and the rest of the frame still draws"
    );
}

/// Coordinates are bit patterns, not numbers to be converted.
///
/// The GL uniform path learned this from a test that pinned NaN bits; the same
/// reasoning applies here, and a producer that encodes a NaN is entitled to
/// have the renderer see the specification's answer for a NaN rather than for
/// whatever a conversion produced.
#[test]
fn coordinates_survive_as_exact_bit_patterns() {
    let nan = f32::NAN.to_bits();
    let negative_zero = (-0.0f32).to_bits();
    let words = stream_of(&[
        record(OP2D_SELECT_CANVAS, &[1]),
        record(OP2D_MOVE_TO, &[nan, negative_zero]),
        record(
            OP2D_SET_FILL_STYLE,
            &[
                0.25f32.to_bits(),
                0.5f32.to_bits(),
                0.75f32.to_bits(),
                1.0f32.to_bits(),
            ],
        ),
    ]);

    let (ops, _) = decode(&words);
    let FrameOp::CanvasBatch(batch) = &ops[0] else {
        panic!("expected a canvas batch");
    };
    match &batch.commands[0] {
        Canvas2DCmd::MoveTo { x, y } => {
            assert_eq!(x.to_bits(), nan, "NaN payload survives");
            assert_eq!(y.to_bits(), negative_zero, "negative zero is not zero");
        }
        other => panic!("expected MoveTo, got {other:?}"),
    }
    match &batch.commands[1] {
        Canvas2DCmd::SetFillStyle { color } => {
            assert_eq!((color.r, color.g, color.b, color.a), (0.25, 0.5, 0.75, 1.0));
        }
        other => panic!("expected SetFillStyle, got {other:?}"),
    }
}

/// The `counterclockwise` flag is a bool on the wire, and the validator refuses
/// anything but 0 or 1 -- so the decoder can read it without re-checking, and
/// this is the assertion that the two agree.
#[test]
fn the_arc_direction_flag_is_a_real_bool() {
    for (raw, expected) in [(0u32, false), (1u32, true)] {
        let words = stream_of(&[
            record(OP2D_SELECT_CANVAS, &[1]),
            record(
                OP2D_ARC,
                &[0, 0, 8f32.to_bits(), 0, std::f32::consts::PI.to_bits(), raw],
            ),
        ]);
        let (ops, _) = decode(&words);
        let FrameOp::CanvasBatch(batch) = &ops[0] else {
            panic!("expected a canvas batch");
        };
        assert!(matches!(
            batch.commands[0],
            Canvas2DCmd::Arc { counterclockwise, .. } if counterclockwise == expected
        ));
    }

    // Two is not a truthy value here; the validator refuses the record.
    let words = stream_of(&[
        record(OP2D_SELECT_CANVAS, &[1]),
        record(
            OP2D_ARC,
            &[0, 0, 8f32.to_bits(), 0, std::f32::consts::PI.to_bits(), 2],
        ),
    ]);
    assert!(
        validate_stream(&words, words.len() as u32).is_err(),
        "a bool word outside 0..=1 is a malformed record"
    );
}

/// Every opcode the block declares has a spec, and every spec is reachable.
#[test]
fn the_block_is_contiguous_and_fully_specified() {
    for opcode in OP2D_BASE..OP2D_END {
        assert!(
            frame_wire::canvas2d::record_spec(opcode).is_some(),
            "opcode {opcode} is inside the block and has no spec"
        );
    }
    assert!(
        frame_wire::canvas2d::record_spec(OP2D_END).is_none(),
        "the block ends where it says it does"
    );
    assert!(
        frame_wire::stream::record_spec(OP2D_MOVE_TO).is_some(),
        "the shared validator dispatches 2D opcodes to the 2D table"
    );
}

/// The canvas's own lifetime, which only this lane carries: in process the
/// engine's facade reaches three ops on the render thread's FIFO, and here the
/// create has to be *in* the run that draws, or the draws arrive for a canvas
/// that does not exist and are dropped with nothing red.
#[test]
fn a_canvas_is_created_resized_and_destroyed_inside_the_run_that_draws_on_it() {
    let canvas = 1 << 24;
    let (ops, context) = decode(&stream_of(&[
        record(OP2D_SELECT_CANVAS, &[canvas]),
        record(OP2D_REGISTER_CANVAS, &[64, 32]),
        record(OP2D_CREATE_CONTEXT, &[]),
        record(OP2D_FILL_RECT, &[0, 0, 0, 0]),
        record(OP2D_RESIZE_CANVAS, &[RESIZE_CANVAS_WIDTH, 128, 0]),
        record(OP2D_DESTROY_CANVAS, &[]),
    ]));
    assert!(context.errors.is_empty(), "no WebGL error belongs here");
    // The trailing materialize is the decoder's standing rule for 2D work a
    // frame ends with, not anything these records ask for.
    assert_eq!(
        shape(&ops),
        vec![
            format!("CanvasBatch({canvas}, 5)"),
            format!("Materialize({canvas})")
        ]
    );
    let FrameOp::CanvasBatch(batch) = &ops[0] else {
        panic!("the run is one batch");
    };
    let commands: Vec<String> = batch
        .commands
        .iter()
        .map(|command| format!("{command:?}"))
        .collect();
    assert_eq!(
        commands,
        vec![
            "RegisterCanvas { width: 64, height: 32 }".to_string(),
            "CreateContext2D".to_string(),
            "FillRect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 }".to_string(),
            "ResizeCanvas { w: Some(128), h: None }".to_string(),
            "DestroyCanvas".to_string(),
        ],
        "the lifetime records decode in place, between the draws they bracket"
    );
}

/// A resize names width, height or both. The envelope cannot check that -- a
/// word count says how many numbers arrived and a bool position says which are
/// 0 or 1 -- so the reader does, and a record naming nothing is a stream it does
/// not execute rather than a resize applied to a guess.
#[test]
fn a_resize_that_names_no_dimension_or_an_unknown_one_is_refused() {
    for flags in [0, 4, RESIZE_CANVAS_WIDTH | 8, u32::MAX] {
        assert!(
            frame_decode::canvas2d::decode_record(
                OP2D_RESIZE_CANVAS,
                &[pack_header(OP2D_RESIZE_CANVAS, 4), flags, 16, 16]
            )
            .is_none(),
            "a resize with flags {flags} names a dimension this reader does not have"
        );
    }
    for (flags, expected) in [
        (RESIZE_CANVAS_WIDTH, (Some(16u32), None)),
        (RESIZE_CANVAS_HEIGHT, (None, Some(9u32))),
        (
            RESIZE_CANVAS_WIDTH | RESIZE_CANVAS_HEIGHT,
            (Some(16), Some(9)),
        ),
    ] {
        let decoded = frame_decode::canvas2d::decode_record(
            OP2D_RESIZE_CANVAS,
            &[pack_header(OP2D_RESIZE_CANVAS, 4), flags, 16, 9],
        )
        .expect("a resize naming a dimension this reader has");
        assert!(
            matches!(decoded, Canvas2DCmd::ResizeCanvas { w, h } if (w, h) == expected),
            "flags {flags} decoded as {decoded:?}"
        );
    }
}
