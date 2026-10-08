//! The fixed-function state records (GL 20..=39): a record decodes into its command, and one whose value the call does
//! not take is refused with the error WebGL names and nothing forwarded -- a comparison function, face, stencil
//! operation or blend factor that is none (INVALID_ENUM), a constant colour factor with a constant alpha one among the
//! colour factors (INVALID_OPERATION, WebGL 1.0 6.13), a line width that is not above 0 (INVALID_VALUE), a depth range
//! whose near value is past its far one (INVALID_OPERATION, WebGL 1.0 6.12). The driver would refuse most of these
//! itself, but its error never reaches `getError`.

use frame_decode::{GlDecodeContext, RenderSink, decode_render_stream_into};
use frame_wire::gl::{
    OP_BLEND_FUNC, OP_BLEND_FUNC_SEPARATE, OP_CULL_FACE, OP_DEPTH_FUNC, OP_DEPTH_RANGE,
    OP_FRONT_FACE, OP_LINE_WIDTH, OP_STENCIL_FUNC, OP_STENCIL_FUNC_SEPARATE,
    OP_STENCIL_MASK_SEPARATE, OP_STENCIL_OP, OP_STENCIL_OP_SEPARATE,
};
use frame_wire::stream::{MAGIC, STREAM_VERSION, pack_header, validate_stream};
use shared::command_vec_pool::PooledVec;
use shared::protocol::render_cmd::{Canvas2DCmd, GLCmd};

const INVALID_ENUM: u32 = 0x0500;
const INVALID_VALUE: u32 = 0x0501;
const INVALID_OPERATION: u32 = 0x0502;
const FRONT: u32 = 0x0404;
const BACK: u32 = 0x0405;
const FRONT_AND_BACK: u32 = 0x0408;
const KEEP: u32 = 0x1E00;
const ALWAYS: u32 = 0x0207;
const CONSTANT_COLOR: u32 = 0x8001;
const ONE_MINUS_CONSTANT_ALPHA: u32 = 0x8004;
const SRC_ALPHA_SATURATE: u32 = 0x0308;

#[derive(Default)]
struct Recorder {
    commands: Vec<GLCmd>,
    errors: Vec<u32>,
}

impl GlDecodeContext for Recorder {
    fn image_source(&mut self, _: u32) -> Option<shared::protocol::render_cmd::TextureSource> {
        None
    }
    fn push_error(&mut self, _canvas_id: u32, code: u32) {
        self.errors.push(code);
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
    fn staged_payload(&mut self) -> Option<&mut frame_decode::StagedPayload> {
        None
    }
}

impl RenderSink for Recorder {
    fn canvas_batch(&mut self, _canvas_id: u32, _commands: PooledVec<Canvas2DCmd>) {}
    fn gl_batch(&mut self, commands: PooledVec<GLCmd>, _approx_bytes: usize) {
        self.commands.extend(commands);
    }
    fn materialize(&mut self, _canvas_id: u32) {}
}

/// The record `opcode` of canvas 7 with `words` after the canvas: how many commands it became, and the errors.
fn decode(opcode: u32, words: &[u32]) -> (usize, Vec<u32>) {
    let mut stream = vec![
        MAGIC,
        STREAM_VERSION,
        pack_header(opcode, words.len() as u32 + 2),
        7,
    ];
    stream.extend_from_slice(words);
    let validated = validate_stream(&stream, stream.len() as u32).expect("structurally valid");
    let mut recorder = Recorder::default();
    decode_render_stream_into(&mut recorder, validated);
    (recorder.commands.len(), recorder.errors)
}

fn taken(opcode: u32, words: &[u32]) {
    assert_eq!(
        decode(opcode, words),
        (1, vec![]),
        "op {opcode} {words:x?} is taken"
    );
}

fn refused(opcode: u32, words: &[u32], error: u32) {
    assert_eq!(
        decode(opcode, words),
        (0, vec![error]),
        "op {opcode} {words:x?} is refused"
    );
}

#[test]
fn comparison_functions_faces_and_stencil_operations_are_the_calls_own() {
    for func in 0x0200..=0x0207 {
        taken(OP_DEPTH_FUNC, &[func]);
        taken(OP_STENCIL_FUNC, &[func, 1, 0xff]);
    }
    refused(OP_DEPTH_FUNC, &[0x0208], INVALID_ENUM);
    refused(OP_DEPTH_FUNC, &[0x01ff], INVALID_ENUM);
    refused(OP_STENCIL_FUNC, &[0x1234, 1, 0xff], INVALID_ENUM);
    for face in [FRONT, BACK, FRONT_AND_BACK] {
        taken(OP_CULL_FACE, &[face]);
        taken(OP_STENCIL_FUNC_SEPARATE, &[face, ALWAYS, 1, 0xff]);
        taken(OP_STENCIL_OP_SEPARATE, &[face, KEEP, 0x8507, 0x150A]);
        taken(OP_STENCIL_MASK_SEPARATE, &[face, 0xff]);
    }
    refused(OP_CULL_FACE, &[0x0900], INVALID_ENUM);
    refused(
        OP_STENCIL_FUNC_SEPARATE,
        &[0x0900, ALWAYS, 1, 0xff],
        INVALID_ENUM,
    );
    refused(
        OP_STENCIL_FUNC_SEPARATE,
        &[FRONT, 0x1234, 1, 0xff],
        INVALID_ENUM,
    );
    refused(
        OP_STENCIL_OP_SEPARATE,
        &[0x0406, KEEP, KEEP, KEEP],
        INVALID_ENUM,
    );
    refused(OP_STENCIL_MASK_SEPARATE, &[0x1234, 0xff], INVALID_ENUM);
    taken(OP_FRONT_FACE, &[0x0900]);
    taken(OP_FRONT_FACE, &[0x0901]);
    refused(OP_FRONT_FACE, &[BACK], INVALID_ENUM);
    for op in [KEEP, 0, 0x1E01, 0x1E02, 0x1E03, 0x150A, 0x8507, 0x8508] {
        taken(OP_STENCIL_OP, &[op, op, op]);
    }
    refused(OP_STENCIL_OP, &[KEEP, 0x1234, KEEP], INVALID_ENUM);
    refused(OP_STENCIL_OP, &[KEEP, KEEP, 1], INVALID_ENUM);
}

#[test]
fn blend_factors_are_the_calls_own_and_constant_colour_and_alpha_do_not_meet() {
    for factor in [
        0,
        1,
        0x0300,
        0x0301,
        0x0302,
        0x0303,
        0x0304,
        0x0305,
        0x0306,
        0x0307,
        SRC_ALPHA_SATURATE,
        0x8001,
        0x8002,
        0x8003,
        0x8004,
    ] {
        taken(OP_BLEND_FUNC, &[factor, 1]);
        taken(OP_BLEND_FUNC, &[1, factor]);
        taken(OP_BLEND_FUNC_SEPARATE, &[1, 0, factor, factor]);
    }
    refused(OP_BLEND_FUNC, &[0x1234, 1], INVALID_ENUM);
    refused(OP_BLEND_FUNC, &[2, 1], INVALID_ENUM);
    refused(OP_BLEND_FUNC_SEPARATE, &[1, 1, 1, 0x0309], INVALID_ENUM);
    refused(
        OP_BLEND_FUNC,
        &[CONSTANT_COLOR, ONE_MINUS_CONSTANT_ALPHA],
        INVALID_OPERATION,
    );
    refused(OP_BLEND_FUNC, &[0x8003, 0x8002], INVALID_OPERATION);
    refused(
        OP_BLEND_FUNC_SEPARATE,
        &[0x8002, 0x8003, 1, 1],
        INVALID_OPERATION,
    );
    taken(
        OP_BLEND_FUNC_SEPARATE,
        &[1, 1, CONSTANT_COLOR, ONE_MINUS_CONSTANT_ALPHA],
    );
    taken(OP_BLEND_FUNC, &[CONSTANT_COLOR, CONSTANT_COLOR]);
    // An unknown factor is INVALID_ENUM before the constant rule is judged.
    refused(
        OP_BLEND_FUNC_SEPARATE,
        &[CONSTANT_COLOR, 0x8003, 1, 0x1234],
        INVALID_ENUM,
    );
}

#[test]
fn a_line_width_is_above_0_and_a_depth_range_runs_near_to_far() {
    taken(OP_LINE_WIDTH, &[2.5f32.to_bits()]);
    refused(OP_LINE_WIDTH, &[0f32.to_bits()], INVALID_VALUE);
    refused(OP_LINE_WIDTH, &[(-1f32).to_bits()], INVALID_VALUE);
    refused(OP_LINE_WIDTH, &[f32::NAN.to_bits()], INVALID_VALUE);
    taken(OP_DEPTH_RANGE, &[0.25f32.to_bits(), 0.25f32.to_bits()]);
    taken(OP_DEPTH_RANGE, &[(-1f32).to_bits(), 2f32.to_bits()]);
    refused(
        OP_DEPTH_RANGE,
        &[1f32.to_bits(), 0f32.to_bits()],
        INVALID_OPERATION,
    );
}
