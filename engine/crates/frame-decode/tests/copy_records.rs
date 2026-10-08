//! The copies (GL 67..=70) and the two WebGL 2 framebuffer records of the resource block (175, 206).
//!
//! A record decodes into exactly the command the call means, and one that breaks a rule the call has without looking
//! at any state is refused with the error the specification names and nothing forwarded: a target or format the call
//! does not take is INVALID_ENUM (a depth or stencil format INVALID_OPERATION), a negative level, offset or size, a
//! border other than 0 and a cube face that is not square INVALID_VALUE.

use frame_decode::{GlDecodeContext, RenderSink, decode_render_stream_into};
use frame_wire::gl::{
    OP_COPY_BUFFER_SUB_DATA, OP_COPY_TEX_IMAGE_2D, OP_COPY_TEX_SUB_IMAGE_2D,
    OP_COPY_TEX_SUB_IMAGE_3D,
};
use frame_wire::gl_resource::{OPR_FRAMEBUFFER_TEXTURE_LAYER, OPR_INVALIDATE_SUB_FRAMEBUFFER};
use frame_wire::stream::{MAGIC, STREAM_VERSION, pack_header, validate_stream};
use shared::command_vec_pool::PooledVec;
use shared::protocol::render_cmd::{Canvas2DCmd, GLCmd};

const TEXTURE_2D: u32 = 0x0DE1;
const CUBE_POSITIVE_Y: u32 = 0x8517;
const TEXTURE_3D: u32 = 0x806F;
const TEXTURE_2D_ARRAY: u32 = 0x8C1A;
const RGBA: u32 = 0x1908;
const RGBA8UI: u32 = 0x8D7C;
const ARRAY_BUFFER: u32 = 0x8892;
const COPY_WRITE_BUFFER: u32 = 0x8F37;
const INVALID_ENUM: u32 = 0x0500;
const INVALID_VALUE: u32 = 0x0501;
const INVALID_OPERATION: u32 = 0x0502;

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

fn i(v: i32) -> u32 {
    v as u32
}

/// `copyTexImage2D` on canvas 3: target, level, internalformat, x, y, width, height, border.
fn copy_tex_image(target: u32, level: i32, format: u32, w: i32, h: i32, border: i32) -> Recorder {
    decode_one(
        OP_COPY_TEX_IMAGE_2D,
        &[3, target, i(level), format, i(-2), 5, i(w), i(h), i(border)],
    )
}

#[test]
fn the_copies_decode_into_their_commands() {
    let r = copy_tex_image(TEXTURE_2D, 1, RGBA8UI, 16, 8, 0);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(
        matches!(
            r.commands.as_slice(),
            [GLCmd::CopyTexImage2D {
                canvas_id: 3,
                target: TEXTURE_2D,
                level: 1,
                internalformat: RGBA8UI,
                x: -2,
                y: 5,
                width: 16,
                height: 8
            }]
        ),
        "{:?}",
        r.commands
    );
    let r = decode_one(
        OP_COPY_TEX_SUB_IMAGE_2D,
        &[3, CUBE_POSITIVE_Y, 0, 1, 2, i(-3), 4, 5, 6],
    );
    assert!(
        matches!(
            r.commands.as_slice(),
            [GLCmd::CopyTexSubImage2D {
                canvas_id: 3,
                target: CUBE_POSITIVE_Y,
                level: 0,
                xoffset: 1,
                yoffset: 2,
                x: -3,
                y: 4,
                width: 5,
                height: 6
            }]
        ),
        "{:?}",
        r.commands
    );
    let r = decode_one(
        OP_COPY_TEX_SUB_IMAGE_3D,
        &[3, TEXTURE_2D_ARRAY, 2, 1, 2, 7, 8, 9, 10, 11],
    );
    assert!(
        matches!(
            r.commands.as_slice(),
            [GLCmd::CopyTexSubImage3D {
                canvas_id: 3,
                target: TEXTURE_2D_ARRAY,
                level: 2,
                xoffset: 1,
                yoffset: 2,
                zoffset: 7,
                x: 8,
                y: 9,
                width: 10,
                height: 11
            }]
        ),
        "{:?}",
        r.commands
    );
    let r = decode_one(
        OP_COPY_BUFFER_SUB_DATA,
        &[3, ARRAY_BUFFER, COPY_WRITE_BUFFER, 4, 8, 12],
    );
    assert!(
        matches!(
            r.commands.as_slice(),
            [GLCmd::CopyBufferSubData {
                canvas_id: 3,
                read_target: ARRAY_BUFFER,
                write_target: COPY_WRITE_BUFFER,
                read_offset: 4,
                write_offset: 8,
                size: 12
            }]
        ),
        "{:?}",
        r.commands
    );
}

#[test]
fn copy_tex_image_2d_refuses_what_the_call_does_not_take() {
    for (target, level, format, w, h, border, want) in [
        (TEXTURE_3D, 0, RGBA, 4, 4, 0, INVALID_ENUM), // a 3D target
        (TEXTURE_2D, 0, 0x1234, 4, 4, 0, INVALID_ENUM), // not a format
        (TEXTURE_2D, 0, 0x81A5, 4, 4, 0, INVALID_OPERATION), // DEPTH_COMPONENT16
        (TEXTURE_2D, 0, 0x88F0, 4, 4, 0, INVALID_OPERATION), // DEPTH24_STENCIL8
        (TEXTURE_2D, -1, RGBA, 4, 4, 0, INVALID_VALUE),
        (TEXTURE_2D, 0, RGBA, -1, 4, 0, INVALID_VALUE),
        (TEXTURE_2D, 0, RGBA, 4, -1, 0, INVALID_VALUE),
        (TEXTURE_2D, 0, RGBA, 4, 4, 1, INVALID_VALUE), // border
        (CUBE_POSITIVE_Y, 0, RGBA, 4, 2, 0, INVALID_VALUE), // a cube face is square
        (TEXTURE_3D, 0, 0x1234, -1, 4, 1, INVALID_ENUM), // the target is checked first
        (TEXTURE_2D, 0, 0x1234, -1, 4, 0, INVALID_ENUM), // then the format
        (TEXTURE_2D, -1, 0x81A5, 4, 4, 0, INVALID_VALUE), // a value before the depth format's operation
    ] {
        let r = copy_tex_image(target, level, format, w, h, border);
        assert_eq!(
            r.errors,
            vec![(3, want)],
            "{target:#x} {level} {format:#x} {w}x{h} border {border}"
        );
        assert!(r.commands.is_empty(), "{:?}", r.commands);
    }
    // the five unsized formats and the sized ones, including integer and float colour
    for format in [
        0x1906, 0x1907, 0x1908, 0x1909, 0x190A, 0x8058, 0x8229, 0x8D62, 0x8C43, 0x8D8E, 0x906F,
        0x881A, 0x8C3A,
    ] {
        let r = copy_tex_image(CUBE_POSITIVE_Y, 0, format, 4, 4, 0);
        assert!(
            r.errors.is_empty() && r.commands.len() == 1,
            "{format:#x}: {:?}",
            r.errors
        );
    }
}

#[test]
fn the_sub_copies_refuse_a_wrong_target_and_negative_values() {
    for (opcode, words, want) in [
        (
            OP_COPY_TEX_SUB_IMAGE_2D,
            vec![3, TEXTURE_3D, 0, 0, 0, 0, 0, 1, 1],
            INVALID_ENUM,
        ),
        (
            OP_COPY_TEX_SUB_IMAGE_2D,
            vec![3, TEXTURE_2D, i(-1), 0, 0, 0, 0, 1, 1],
            INVALID_VALUE,
        ),
        (
            OP_COPY_TEX_SUB_IMAGE_2D,
            vec![3, TEXTURE_2D, 0, i(-1), 0, 0, 0, 1, 1],
            INVALID_VALUE,
        ),
        (
            OP_COPY_TEX_SUB_IMAGE_2D,
            vec![3, TEXTURE_2D, 0, 0, 0, 0, 0, 1, i(-1)],
            INVALID_VALUE,
        ),
        (
            OP_COPY_TEX_SUB_IMAGE_3D,
            vec![3, TEXTURE_2D, 0, 0, 0, 0, 0, 0, 1, 1],
            INVALID_ENUM,
        ),
        (
            OP_COPY_TEX_SUB_IMAGE_3D,
            vec![3, TEXTURE_3D, 0, 0, 0, i(-1), 0, 0, 1, 1],
            INVALID_VALUE,
        ),
        (
            OP_COPY_TEX_SUB_IMAGE_3D,
            vec![3, TEXTURE_3D, 0, 0, 0, 0, 0, 0, i(-4), 1],
            INVALID_VALUE,
        ),
        (
            OP_COPY_BUFFER_SUB_DATA,
            vec![3, 0x1234, ARRAY_BUFFER, 0, 0, 4],
            INVALID_ENUM,
        ),
        (
            OP_COPY_BUFFER_SUB_DATA,
            vec![3, ARRAY_BUFFER, 0x0DE1, 0, 0, 4],
            INVALID_ENUM,
        ),
        (
            OP_COPY_BUFFER_SUB_DATA,
            vec![3, ARRAY_BUFFER, COPY_WRITE_BUFFER, i(-1), 0, 4],
            INVALID_VALUE,
        ),
        (
            OP_COPY_BUFFER_SUB_DATA,
            vec![3, ARRAY_BUFFER, COPY_WRITE_BUFFER, 0, 0, i(-4)],
            INVALID_VALUE,
        ),
    ] {
        let r = decode_one(opcode, &words);
        assert_eq!(r.errors, vec![(3, want)], "opcode {opcode} {words:?}");
        assert!(r.commands.is_empty(), "{:?}", r.commands);
    }
    // the x and y of the read rectangle may be negative: they read outside the framebuffer, which is defined
    let r = decode_one(
        OP_COPY_TEX_SUB_IMAGE_2D,
        &[3, TEXTURE_2D, 0, 0, 0, i(-5), i(-5), 1, 1],
    );
    assert!(
        r.errors.is_empty() && r.commands.len() == 1,
        "{:?}",
        r.errors
    );
}

#[test]
fn the_framebuffer_layer_and_sub_invalidation_records_decode_into_their_commands() {
    let r = decode_one(OPR_FRAMEBUFFER_TEXTURE_LAYER, &[3, 0x8D40, 0x8CE1, 9, 2, 5]);
    assert!(
        matches!(
            r.commands.as_slice(),
            [GLCmd::FramebufferTextureLayer {
                canvas_id: 3,
                target: 0x8D40,
                attachment: 0x8CE1,
                texture: Some(9),
                level: 2,
                layer: 5
            }]
        ),
        "{:?}",
        r.commands
    );
    let r = decode_one(
        OPR_FRAMEBUFFER_TEXTURE_LAYER,
        &[3, 0x8D40, 0x8CE1, i(-1), 0, 0],
    );
    assert!(
        matches!(
            r.commands.as_slice(),
            [GLCmd::FramebufferTextureLayer { texture: None, .. }]
        ),
        "{:?}",
        r.commands
    );
    let r = decode_one(
        OPR_INVALIDATE_SUB_FRAMEBUFFER,
        &[3, 0x8CA9, 1, 2, 3, 4, 2, 0x8CE0, 0x8D00],
    );
    match r.commands.as_slice() {
        [
            GLCmd::InvalidateSubFramebuffer {
                canvas_id: 3,
                target: 0x8CA9,
                attachments,
                x: 1,
                y: 2,
                width: 3,
                height: 4,
            },
        ] => {
            assert_eq!(attachments.as_slice(), &[0x8CE0, 0x8D00]);
        }
        other => panic!("{other:?}"),
    }
}
