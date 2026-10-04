//! `sampleCoverage` (GL 71), `flush` (GL 72), `detachShader` (177) and `validateProgram` (178): each decodes into its
//! command.

use frame_decode::{GlDecodeContext, RenderSink, decode_render_stream_into};
use frame_wire::gl::{OP_FLUSH, OP_SAMPLE_COVERAGE, OP_WEBGL_CONTEXT};
use frame_wire::gl_resource::{OPR_DETACH_SHADER, OPR_VALIDATE_PROGRAM};
use frame_wire::stream::{MAGIC, STREAM_VERSION, pack_header, validate_stream};
use shared::command_vec_pool::PooledVec;
use shared::protocol::render_cmd::{Canvas2DCmd, GLCmd};

#[derive(Default)]
struct Recorder {
    commands: Vec<GLCmd>,
    errors: Vec<(u32, u32)>,
}

impl GlDecodeContext for Recorder {
    fn image_source(&mut self, _: u32) -> Option<shared::protocol::render_cmd::TextureSource> {
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

fn decode_one(opcode: u32, words: &[u32]) -> Recorder {
    let mut stream = vec![
        MAGIC,
        STREAM_VERSION,
        pack_header(opcode, words.len() as u32 + 1),
    ];
    stream.extend_from_slice(words);
    let validated = validate_stream(&stream, stream.len() as u32).expect("structurally valid");
    let mut recorder = Recorder::default();
    decode_render_stream_into(&mut recorder, validated);
    recorder
}

#[test]
fn sample_coverage_keeps_its_value_and_its_invert_flag() {
    for (word, invert) in [(0u32, false), (1, true)] {
        let r = decode_one(OP_SAMPLE_COVERAGE, &[3, 0.25f32.to_bits(), word]);
        assert!(
            matches!(r.commands.as_slice(), [GLCmd::SampleCoverage { canvas_id: 3, value, invert: i }] if *value == 0.25 && *i == invert),
            "{:?}",
            r.commands
        );
    }
}

#[test]
fn a_sample_coverage_flag_that_is_not_a_bool_is_refused_by_the_envelope() {
    let mut stream = vec![
        MAGIC,
        STREAM_VERSION,
        pack_header(OP_SAMPLE_COVERAGE, 4),
        3,
        0,
        2,
    ];
    assert!(validate_stream(&stream, stream.len() as u32).is_err());
    stream[5] = 1;
    assert!(validate_stream(&stream, stream.len() as u32).is_ok());
}

#[test]
fn detach_shader_and_validate_program_decode_into_their_commands() {
    let r = decode_one(OPR_DETACH_SHADER, &[7, 9]);
    assert!(
        matches!(
            r.commands.as_slice(),
            [GLCmd::DetachShader {
                program_id: 7,
                shader_id: 9
            }]
        ),
        "{:?}",
        r.commands
    );
    let r = decode_one(OPR_VALIDATE_PROGRAM, &[7]);
    assert!(
        matches!(
            r.commands.as_slice(),
            [GLCmd::ValidateProgram { program_id: 7 }]
        ),
        "{:?}",
        r.commands
    );
}

#[test]
fn flush_names_its_canvas() {
    let r = decode_one(OP_FLUSH, &[7]);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(
        matches!(r.commands.as_slice(), [GLCmd::Flush { canvas_id: 7 }]),
        "{:?}",
        r.commands
    );
}

/// A WebGL context's record names the buffers its drawing buffer has, one bit each: alpha 1, depth 2, stencil 4,
/// preserveDrawingBuffer 8.
#[test]
fn a_webgl_context_record_names_its_drawing_buffer() {
    for (bits, want) in [
        (0b0011, (true, true, false, false)),
        (0b1100, (false, false, true, true)),
        (0b0000, (false, false, false, false)),
    ] {
        let r = decode_one(OP_WEBGL_CONTEXT, &[9, bits]);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        match r.commands.as_slice() {
            [
                GLCmd::WebglContext {
                    canvas_id: 9,
                    alpha,
                    depth,
                    stencil,
                    preserve_drawing_buffer,
                },
            ] => assert_eq!(
                (*alpha, *depth, *stencil, *preserve_drawing_buffer),
                want,
                "{bits:#b}"
            ),
            other => panic!("expected one WebglContext, got {other:?}"),
        }
    }
}
