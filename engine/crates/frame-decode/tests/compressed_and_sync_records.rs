//! The compressed uploads (resource records 198, 199, 207, 208) and `waitSync` (176).
//!
//! A compressed upload's bytes are its own or -- WebGL 2's other overload -- a range of the bound PIXEL_UNPACK_BUFFER,
//! named by the record's `pbo_offset` / `pbo_size` words (an offset of -1 is none). Each decodes into the command with
//! that source; a buffer range of negative size is INVALID_VALUE and nothing.

use frame_decode::{GlDecodeContext, RenderSink, decode_render_stream_into};
use frame_wire::gl_resource::{
    OPR_COMPRESSED_TEX_IMAGE_2D, OPR_COMPRESSED_TEX_IMAGE_3D, OPR_COMPRESSED_TEX_SUB_IMAGE_2D,
    OPR_COMPRESSED_TEX_SUB_IMAGE_3D, OPR_WAIT_SYNC,
};
use frame_wire::stream::{MAGIC, STREAM_VERSION, pack_header, validate_stream};
use shared::command_vec_pool::PooledVec;
use shared::protocol::render_cmd::{Canvas2DCmd, CompressedImageData, GLCmd};

const TEXTURE_2D: u32 = 0x0DE1;
const TEXTURE_2D_ARRAY: u32 = 0x8C1A;
const ETC2_RGBA8: u32 = 0x9278;
const INVALID_VALUE: u32 = 0x0501;

#[derive(Default)]
struct Recorder {
    commands: Vec<GLCmd>,
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

/// `prefix` words (canvas 3 first), then the byte payload: its length and the bytes padded to a word.
fn with_bytes(opcode: u32, prefix: &[u32], bytes: &[u8]) -> Recorder {
    let mut words = prefix.to_vec();
    words.push(bytes.len() as u32);
    for chunk in bytes.chunks(4) {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        words.push(u32::from_le_bytes(word));
    }
    decode_one(opcode, &words)
}

const BLOCK: [u8; 16] = [255, 0, 0, 0, 0, 0, 0, 0, 0xF8, 0, 0, 2, 0xFF, 0xFF, 0, 0];

#[test]
fn a_compressed_upload_carries_its_bytes_or_its_buffer_range() {
    // 2D, the bytes
    let r = with_bytes(
        OPR_COMPRESSED_TEX_IMAGE_2D,
        &[3, TEXTURE_2D, 1, ETC2_RGBA8, 4, 4, 0, i(-1), 0],
        &BLOCK,
    );
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    match r.commands.as_slice() {
        [
            GLCmd::CompressedTexImage2D {
                canvas_id: 3,
                target: TEXTURE_2D,
                level: 1,
                internalformat: ETC2_RGBA8,
                width: 4,
                height: 4,
                border: 0,
                data: CompressedImageData::Bytes(bytes),
            },
        ] => {
            assert_eq!(bytes.as_slice(), &BLOCK)
        }
        other => panic!("{other:?}"),
    }
    // 2D, a buffer range: no bytes follow
    let r = with_bytes(
        OPR_COMPRESSED_TEX_IMAGE_2D,
        &[3, TEXTURE_2D, 0, ETC2_RGBA8, 4, 4, 0, 32, 16],
        &[],
    );
    assert!(
        matches!(
            r.commands.as_slice(),
            [GLCmd::CompressedTexImage2D {
                data: CompressedImageData::UnpackBuffer {
                    offset: 32,
                    size: 16
                },
                ..
            }]
        ),
        "{:?}",
        r.commands
    );
    // sub 2D, a buffer range
    let r = with_bytes(
        OPR_COMPRESSED_TEX_SUB_IMAGE_2D,
        &[3, TEXTURE_2D, 0, 4, 8, 4, 4, ETC2_RGBA8, 0, 16],
        &[],
    );
    assert!(
        matches!(
            r.commands.as_slice(),
            [GLCmd::CompressedTexSubImage2D {
                xoffset: 4,
                yoffset: 8,
                format: ETC2_RGBA8,
                data: CompressedImageData::UnpackBuffer {
                    offset: 0,
                    size: 16
                },
                ..
            }]
        ),
        "{:?}",
        r.commands
    );
    // 3D, the bytes
    let mut two = BLOCK.to_vec();
    two.extend_from_slice(&BLOCK);
    let r = with_bytes(
        OPR_COMPRESSED_TEX_IMAGE_3D,
        &[3, TEXTURE_2D_ARRAY, 0, ETC2_RGBA8, 4, 4, 2, 0, i(-1), 0],
        &two,
    );
    match r.commands.as_slice() {
        [
            GLCmd::CompressedTexImage3D {
                target: TEXTURE_2D_ARRAY,
                internalformat: ETC2_RGBA8,
                width: 4,
                height: 4,
                depth: 2,
                border: 0,
                data: CompressedImageData::Bytes(bytes),
                ..
            },
        ] => {
            assert_eq!(bytes.as_slice(), two.as_slice())
        }
        other => panic!("{other:?}"),
    }
    // sub 3D, the bytes and then a buffer range
    let r = with_bytes(
        OPR_COMPRESSED_TEX_SUB_IMAGE_3D,
        &[
            3,
            TEXTURE_2D_ARRAY,
            0,
            0,
            0,
            1,
            4,
            4,
            1,
            ETC2_RGBA8,
            i(-1),
            0,
        ],
        &BLOCK,
    );
    assert!(
        matches!(
            r.commands.as_slice(),
            [GLCmd::CompressedTexSubImage3D {
                zoffset: 1,
                depth: 1,
                format: ETC2_RGBA8,
                data: CompressedImageData::Bytes(_),
                ..
            }]
        ),
        "{:?}",
        r.commands
    );
    let r = with_bytes(
        OPR_COMPRESSED_TEX_SUB_IMAGE_3D,
        &[3, TEXTURE_2D_ARRAY, 0, 0, 0, 1, 4, 4, 1, ETC2_RGBA8, 48, 16],
        &[],
    );
    assert!(
        matches!(
            r.commands.as_slice(),
            [GLCmd::CompressedTexSubImage3D {
                data: CompressedImageData::UnpackBuffer {
                    offset: 48,
                    size: 16
                },
                ..
            }]
        ),
        "{:?}",
        r.commands
    );
}

#[test]
fn a_buffer_range_of_negative_size_is_invalid_value_and_nothing() {
    for (opcode, prefix) in [
        (
            OPR_COMPRESSED_TEX_IMAGE_2D,
            vec![3, TEXTURE_2D, 0, ETC2_RGBA8, 4, 4, 0, 0, i(-16)],
        ),
        (
            OPR_COMPRESSED_TEX_SUB_IMAGE_2D,
            vec![3, TEXTURE_2D, 0, 0, 0, 4, 4, ETC2_RGBA8, 0, i(-1)],
        ),
        (
            OPR_COMPRESSED_TEX_IMAGE_3D,
            vec![3, TEXTURE_2D_ARRAY, 0, ETC2_RGBA8, 4, 4, 1, 0, 0, i(-1)],
        ),
        (
            OPR_COMPRESSED_TEX_SUB_IMAGE_3D,
            vec![
                3,
                TEXTURE_2D_ARRAY,
                0,
                0,
                0,
                0,
                4,
                4,
                1,
                ETC2_RGBA8,
                0,
                i(-1),
            ],
        ),
    ] {
        let r = with_bytes(opcode, &prefix, &[]);
        assert_eq!(r.errors, vec![(3, INVALID_VALUE)], "opcode {opcode}");
        assert!(r.commands.is_empty(), "{:?}", r.commands);
    }
}

#[test]
fn wait_sync_decodes_into_its_command() {
    let r = decode_one(OPR_WAIT_SYNC, &[3, 41]);
    assert!(
        matches!(
            r.commands.as_slice(),
            [GLCmd::WaitSync {
                canvas_id: 3,
                sync: 41
            }]
        ),
        "{:?}",
        r.commands
    );
}
