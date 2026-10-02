//! The `clearBuffer*` records (WebGL 2): opcodes 63..=66.
//!
//! A record decodes into exactly the command the call means, with its values typed as the call types them (a float
//! stays a float, an unsigned word stays unsigned), and one that breaks the rules of its call is refused with the error
//! the specification names and nothing forwarded to the renderer: a buffer enum the call does not take is INVALID_ENUM,
//! a negative draw buffer -- or, for anything but COLOR, a draw buffer other than 0 -- is INVALID_VALUE.

use frame_decode::{GlDecodeContext, RenderSink, decode_render_stream_into};
use frame_wire::gl::{
    OP_CLEAR_BUFFER_FI, OP_CLEAR_BUFFER_FV, OP_CLEAR_BUFFER_IV, OP_CLEAR_BUFFER_UIV,
};
use frame_wire::stream::{MAGIC, STREAM_VERSION, pack_header, validate_stream};
use shared::command_vec_pool::PooledVec;
use shared::protocol::render_cmd::{Canvas2DCmd, GLCmd};

const COLOR: u32 = 0x1800;
const DEPTH: u32 = 0x1801;
const STENCIL: u32 = 0x1802;
const DEPTH_STENCIL: u32 = 0x84F9;
const INVALID_ENUM: u32 = 0x0500;
const INVALID_VALUE: u32 = 0x0501;

#[derive(Default)]
struct Recorder {
    commands: Vec<String>,
    errors: Vec<(u32, u32)>,
}

impl GlDecodeContext for Recorder {
    fn image_upload(&mut self, _upload: frame_decode::ImageUpload) -> Option<GLCmd> {
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
        self.commands
            .extend(commands.iter().map(|cmd| format!("{cmd:?}")));
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

/// canvas 3, buffer, draw buffer, then four value words.
fn four(opcode: u32, buffer: u32, drawbuffer: i32, values: [u32; 4]) -> Recorder {
    let mut words = vec![3, buffer, drawbuffer as u32];
    words.extend_from_slice(&values);
    decode_one(opcode, &words)
}

#[test]
fn the_value_records_decode_into_their_commands_with_typed_values() {
    let f = [
        0.25f32.to_bits(),
        0.5f32.to_bits(),
        (-1.5f32).to_bits(),
        1.0f32.to_bits(),
    ];
    let r = four(OP_CLEAR_BUFFER_FV, COLOR, 2, f);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let text = r.commands.join("");
    assert!(
        text.starts_with("ClearBufferfv")
            && text.contains("drawbuffer: 2")
            && text.contains("[0.25, 0.5, -1.5, 1.0]"),
        "{text}"
    );

    let r = four(OP_CLEAR_BUFFER_IV, STENCIL, 0, [(-7i32) as u32, 0, 0, 0]);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(
        r.commands.join("").contains("ClearBufferiv")
            && r.commands.join("").contains("[-7, 0, 0, 0]"),
        "{:?}",
        r.commands
    );

    // above i32::MAX, to show the words stay unsigned
    let r = four(OP_CLEAR_BUFFER_UIV, COLOR, 1, [u32::MAX, 0x8000_0000, 3, 4]);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(
        r.commands.join("").contains("ClearBufferuiv")
            && r.commands
                .join("")
                .contains("[4294967295, 2147483648, 3, 4]"),
        "{:?}",
        r.commands
    );

    let r = decode_one(
        OP_CLEAR_BUFFER_FI,
        &[3, DEPTH_STENCIL, 0, 0.75f32.to_bits(), 9],
    );
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let text = r.commands.join("");
    assert!(
        text.starts_with("ClearBufferfi")
            && text.contains("depth: 0.75")
            && text.contains("stencil: 9"),
        "{text}"
    );
}

#[test]
fn a_buffer_the_call_does_not_take_is_invalid_enum_and_sends_nothing() {
    let z = [0u32; 4];
    for (opcode, buffer) in [
        (OP_CLEAR_BUFFER_FV, STENCIL), // fv: COLOR or DEPTH
        (OP_CLEAR_BUFFER_FV, DEPTH_STENCIL),
        (OP_CLEAR_BUFFER_IV, DEPTH),  // iv: COLOR or STENCIL
        (OP_CLEAR_BUFFER_UIV, DEPTH), // uiv: COLOR only
        (OP_CLEAR_BUFFER_UIV, STENCIL),
        (OP_CLEAR_BUFFER_FV, 0x1234),
    ] {
        let r = four(opcode, buffer, 0, z);
        assert_eq!(
            r.errors,
            vec![(3, INVALID_ENUM)],
            "opcode {opcode} buffer {buffer:#x}"
        );
        assert!(
            r.commands.is_empty(),
            "opcode {opcode} buffer {buffer:#x}: {:?}",
            r.commands
        );
    }
    for buffer in [COLOR, DEPTH, STENCIL, 0] {
        let r = decode_one(OP_CLEAR_BUFFER_FI, &[3, buffer, 0, 0, 0]);
        assert_eq!(r.errors, vec![(3, INVALID_ENUM)], "fi buffer {buffer:#x}");
        assert!(r.commands.is_empty());
    }
}

#[test]
fn a_draw_buffer_out_of_range_is_invalid_value_and_sends_nothing() {
    let z = [0u32; 4];
    // negative anywhere; non-zero for anything but COLOR
    for (opcode, buffer, drawbuffer) in [
        (OP_CLEAR_BUFFER_FV, COLOR, -1),
        (OP_CLEAR_BUFFER_FV, DEPTH, 1),
        (OP_CLEAR_BUFFER_IV, STENCIL, 2),
        (OP_CLEAR_BUFFER_UIV, COLOR, -5),
    ] {
        let r = four(opcode, buffer, drawbuffer, z);
        assert_eq!(
            r.errors,
            vec![(3, INVALID_VALUE)],
            "opcode {opcode} buffer {buffer:#x} drawbuffer {drawbuffer}"
        );
        assert!(r.commands.is_empty(), "{:?}", r.commands);
    }
    let r = decode_one(OP_CLEAR_BUFFER_FI, &[3, DEPTH_STENCIL, 1, 0, 0]);
    assert_eq!(r.errors, vec![(3, INVALID_VALUE)]);
    assert!(r.commands.is_empty());
    // COLOR's draw buffer may be any non-negative number: its upper bound is the device's, not the decoder's.
    let r = four(OP_CLEAR_BUFFER_FV, COLOR, 7, z);
    assert!(
        r.errors.is_empty() && r.commands.len() == 1,
        "{:?} {:?}",
        r.errors,
        r.commands
    );
}
