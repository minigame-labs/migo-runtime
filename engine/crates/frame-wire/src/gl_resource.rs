//! The WebGL resource block: object lifetimes, shader and program set-up, and
//! the uploads that carry bytes. Opcodes 128..=191 fixed, 192..=255 carrying a
//! payload.
//!
//! # Why this block exists
//!
//! In process, the engine's WebGL facade encodes per-frame calls into the GL
//! block and makes these calls as ops: `createBuffer`, `shaderSource`,
//! `texImage2D` run rarely per frame and carry strings and bytes, so an op was
//! the cheaper shape. On the Apple Performance+ lane the facade runs in WebKit's
//! WebContent process and every call has to be bytes, so each of those ops is a
//! record here -- written by the producer's stream lane
//! (`platforms/apple/WebContent/PerformancePlus/src/lane-stream.mjs`) in the
//! order the facade made the calls, and decoded into exactly the `GLCmd` the op
//! builds (`frame_decode::resource`, which the ops call too).
//!
//! # Record layout
//!
//! A record's words are its op's arguments, in the op's order, after the
//! header. Where the op takes a canvas id it is the first argument, as it is in
//! the op. A payload record ends with `byte_length` and the bytes padded to a
//! word with zero bytes, or with `count` and that many words; the envelope checks
//! both (see [`crate::stream::RecordSpec::Bytes`] and `Words`).
//!
//! The in-process encoder does not carry this block: those calls stay ops there.
//! `scripts/test-render-opcode-agreement.sh` knows that and checks the two
//! encoders that do.

use crate::stream::RecordSpec;

/// The first opcode of this block.
pub const OPR_BASE: u32 = 128;
/// The first payload-carrying opcode.
pub const OPR_PAYLOAD_BASE: u32 = 192;
/// One past the last opcode this block may ever use.
pub const OPR_END: u32 = 256;

/// The most words a `Words` record may carry. `drawBuffers` and
/// `invalidateFramebuffer` name attachments, of which GL ES 3.0 has at most 16
/// colour plus depth and stencil; this is generous and still bounded.
pub const MAX_RESOURCE_WORD_LIST: u32 = 64;

// ─── Fixed-arity records ─────────────────────────────────────────────────────

// H C client
pub const OPR_CREATE_BUFFER: u32 = 128;
pub const OPR_CREATE_FRAMEBUFFER: u32 = 129;
pub const OPR_CREATE_PROGRAM: u32 = 130;
pub const OPR_CREATE_QUERY: u32 = 131;
pub const OPR_CREATE_RENDERBUFFER: u32 = 132;
pub const OPR_CREATE_SAMPLER: u32 = 133;
// H C client type
pub const OPR_CREATE_SHADER: u32 = 134;
// H C client
pub const OPR_CREATE_TEXTURE: u32 = 135;
pub const OPR_CREATE_TRANSFORM_FEEDBACK: u32 = 136;
pub const OPR_CREATE_VERTEX_ARRAY: u32 = 137;
// H id
pub const OPR_DELETE_BUFFER: u32 = 138;
pub const OPR_DELETE_FRAMEBUFFER: u32 = 139;
pub const OPR_DELETE_PROGRAM: u32 = 140;
pub const OPR_DELETE_QUERY: u32 = 141;
pub const OPR_DELETE_RENDERBUFFER: u32 = 142;
pub const OPR_DELETE_SAMPLER: u32 = 143;
pub const OPR_DELETE_SHADER: u32 = 144;
pub const OPR_DELETE_SYNC: u32 = 145;
pub const OPR_DELETE_TEXTURE: u32 = 146;
pub const OPR_DELETE_TRANSFORM_FEEDBACK: u32 = 147;
pub const OPR_DELETE_VERTEX_ARRAY: u32 = 148;
// H program shader
pub const OPR_ATTACH_SHADER: u32 = 149;
// H shader
pub const OPR_COMPILE_SHADER: u32 = 150;
// H program
pub const OPR_LINK_PROGRAM: u32 = 151;
// H C target query
pub const OPR_BEGIN_QUERY: u32 = 152;
// H C target
pub const OPR_END_QUERY: u32 = 153;
// H C primitive_mode
pub const OPR_BEGIN_TRANSFORM_FEEDBACK: u32 = 154;
// H C
pub const OPR_END_TRANSFORM_FEEDBACK: u32 = 155;
pub const OPR_PAUSE_TRANSFORM_FEEDBACK: u32 = 156;
pub const OPR_RESUME_TRANSFORM_FEEDBACK: u32 = 157;
// H C target tf
pub const OPR_BIND_TRANSFORM_FEEDBACK: u32 = 158;
// H C src_x0:I src_y0:I src_x1:I src_y1:I dst_x0:I dst_y0:I dst_x1:I dst_y1:I mask filter
pub const OPR_BLIT_FRAMEBUFFER: u32 = 159;
// H C client condition flags
pub const OPR_FENCE_SYNC: u32 = 160;
// H C target attachment renderbuffertarget renderbuffer:I
pub const OPR_FRAMEBUFFER_RENDERBUFFER: u32 = 161;
// H C target attachment textarget texture:I level:I
pub const OPR_FRAMEBUFFER_TEXTURE_2D: u32 = 162;
// H C target internalformat width:I height:I
pub const OPR_RENDERBUFFER_STORAGE: u32 = 163;
// H C target samples:I internal_format width:I height:I
pub const OPR_RENDERBUFFER_STORAGE_MULTISAMPLE: u32 = 164;
// H C target levels:I internal_format width:I height:I
pub const OPR_TEX_STORAGE_2D: u32 = 165;
// H C target levels:I internal_format width:I height:I depth:I
pub const OPR_TEX_STORAGE_3D: u32 = 166;
// H program uniform_block_index uniform_block_binding
pub const OPR_UNIFORM_BLOCK_BINDING: u32 = 167;
// H C
pub const OPR_LOSE_CONTEXT: u32 = 168;

// ─── Payload records ─────────────────────────────────────────────────────────

// H C shader | len source(UTF-8)
pub const OPR_SHADER_SOURCE: u32 = 192;
// H program index | len name(UTF-8)
pub const OPR_BIND_ATTRIB_LOCATION: u32 = 193;
// H C target size:I usage has_data:B | len data
pub const OPR_BUFFER_DATA: u32 = 194;
// H C target offset:I | len data
pub const OPR_BUFFER_SUB_DATA: u32 = 195;
// H C target level:I internalformat:I width:I height:I border:I format type has_data:B | len data
pub const OPR_TEX_IMAGE_2D: u32 = 196;
// H C target level:I xoffset:I yoffset:I width:I height:I format type | len data
pub const OPR_TEX_SUB_IMAGE_2D: u32 = 197;
// H C target level:I internalformat width:I height:I border:I | len data
pub const OPR_COMPRESSED_TEX_IMAGE_2D: u32 = 198;
// H C target level:I xoffset:I yoffset:I width:I height:I format | len data
pub const OPR_COMPRESSED_TEX_SUB_IMAGE_2D: u32 = 199;
// H C target level:I internal_format:I width:I height:I depth:I border:I format ty
//   pbo_offset:I has_pixels:B | len pixels
pub const OPR_TEX_IMAGE_3D: u32 = 200;
// H C target level:I xoffset:I yoffset:I zoffset:I width:I height:I depth:I format ty
//   pbo_offset:I has_pixels:B | len pixels
pub const OPR_TEX_SUB_IMAGE_3D: u32 = 201;
// H C | count buffers...
pub const OPR_DRAW_BUFFERS: u32 = 202;
// H C target | count attachments...
pub const OPR_INVALIDATE_FRAMEBUFFER: u32 = 203;
// H C program buffer_mode | len names(UTF-8, joined by U+001F)
pub const OPR_TRANSFORM_FEEDBACK_VARYINGS: u32 = 204;

/// The shape of one record in this block.
pub fn record_spec(opcode: u32) -> Option<RecordSpec> {
    const fn fixed(word_count: u32) -> RecordSpec {
        RecordSpec::Fixed {
            word_count,
            bool_words: &[],
        }
    }
    const fn bytes(prefix_words: u8, presence_word: Option<u8>, text: bool) -> RecordSpec {
        RecordSpec::Bytes {
            prefix_words,
            presence_word,
            text,
        }
    }
    Some(match opcode {
        OPR_CREATE_BUFFER
        | OPR_CREATE_FRAMEBUFFER
        | OPR_CREATE_PROGRAM
        | OPR_CREATE_QUERY
        | OPR_CREATE_RENDERBUFFER
        | OPR_CREATE_SAMPLER
        | OPR_CREATE_TEXTURE
        | OPR_CREATE_TRANSFORM_FEEDBACK
        | OPR_CREATE_VERTEX_ARRAY => fixed(3),
        OPR_CREATE_SHADER => fixed(4),
        OPR_DELETE_BUFFER
        | OPR_DELETE_FRAMEBUFFER
        | OPR_DELETE_PROGRAM
        | OPR_DELETE_QUERY
        | OPR_DELETE_RENDERBUFFER
        | OPR_DELETE_SAMPLER
        | OPR_DELETE_SHADER
        | OPR_DELETE_SYNC
        | OPR_DELETE_TEXTURE
        | OPR_DELETE_TRANSFORM_FEEDBACK
        | OPR_DELETE_VERTEX_ARRAY => fixed(2),
        OPR_ATTACH_SHADER => fixed(3),
        OPR_COMPILE_SHADER | OPR_LINK_PROGRAM => fixed(2),
        OPR_BEGIN_QUERY => fixed(4),
        OPR_END_QUERY => fixed(3),
        OPR_BEGIN_TRANSFORM_FEEDBACK => fixed(3),
        OPR_END_TRANSFORM_FEEDBACK
        | OPR_PAUSE_TRANSFORM_FEEDBACK
        | OPR_RESUME_TRANSFORM_FEEDBACK => fixed(2),
        OPR_BIND_TRANSFORM_FEEDBACK => fixed(4),
        OPR_BLIT_FRAMEBUFFER => fixed(12),
        OPR_FENCE_SYNC => fixed(5),
        OPR_FRAMEBUFFER_RENDERBUFFER => fixed(6),
        OPR_FRAMEBUFFER_TEXTURE_2D => fixed(7),
        OPR_RENDERBUFFER_STORAGE => fixed(6),
        OPR_RENDERBUFFER_STORAGE_MULTISAMPLE => fixed(7),
        OPR_TEX_STORAGE_2D => fixed(7),
        OPR_TEX_STORAGE_3D => fixed(8),
        OPR_UNIFORM_BLOCK_BINDING => fixed(4),
        OPR_LOSE_CONTEXT => fixed(2),

        OPR_SHADER_SOURCE => bytes(3, None, true),
        OPR_BIND_ATTRIB_LOCATION => bytes(3, None, true),
        OPR_BUFFER_DATA => bytes(6, Some(5), false),
        OPR_BUFFER_SUB_DATA => bytes(4, None, false),
        OPR_TEX_IMAGE_2D => bytes(11, Some(10), false),
        OPR_TEX_SUB_IMAGE_2D => bytes(10, None, false),
        OPR_COMPRESSED_TEX_IMAGE_2D => bytes(8, None, false),
        OPR_COMPRESSED_TEX_SUB_IMAGE_2D => bytes(9, None, false),
        OPR_TEX_IMAGE_3D => bytes(13, Some(12), false),
        OPR_TEX_SUB_IMAGE_3D => bytes(14, Some(13), false),
        OPR_DRAW_BUFFERS => RecordSpec::Words {
            prefix_words: 2,
            max_count: MAX_RESOURCE_WORD_LIST,
        },
        OPR_INVALIDATE_FRAMEBUFFER => RecordSpec::Words {
            prefix_words: 3,
            max_count: MAX_RESOURCE_WORD_LIST,
        },
        OPR_TRANSFORM_FEEDBACK_VARYINGS => bytes(4, None, true),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every opcode in the block's range either has a spec or is unassigned, and
    /// the payload specs sit in the payload range.
    #[test]
    fn fixed_and_payload_records_sit_in_their_own_ranges() {
        for opcode in OPR_BASE..OPR_END {
            match record_spec(opcode) {
                Some(RecordSpec::Fixed { .. }) => assert!(opcode < OPR_PAYLOAD_BASE, "{opcode}"),
                Some(RecordSpec::Bytes { .. } | RecordSpec::Words { .. }) => {
                    assert!(opcode >= OPR_PAYLOAD_BASE, "{opcode}")
                }
                Some(other) => panic!("{opcode}: unexpected spec {other:?}"),
                None => {}
            }
        }
    }

    /// A presence word lies in the prefix, after the header, and a bool word is
    /// what it is: the envelope checks it is 0 or 1.
    #[test]
    fn presence_words_are_inside_their_prefix() {
        for opcode in OPR_PAYLOAD_BASE..OPR_END {
            if let Some(RecordSpec::Bytes {
                prefix_words,
                presence_word: Some(index),
                ..
            }) = record_spec(opcode)
            {
                assert!(index >= 1 && index < prefix_words, "{opcode}");
            }
        }
    }
}
