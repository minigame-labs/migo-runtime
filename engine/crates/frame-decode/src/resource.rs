//! The WebGL resource block, decoded -- and the builders the in-process ops use
//! for the same calls.
//!
//! Every non-trivial call here is built by one function that both paths call:
//! the embedded runtime's op with the bytes V8 handed it, and the decoder with
//! the words a producer wrote. The limits a call is held to (the upload
//! ceiling, the shader-source ceiling, a negative offset), the error it reports
//! when it is not, and the transform-feedback state it moves are therefore one
//! implementation, not two that have to agree. The trivial calls -- a create,
//! a delete, a bind -- copy their arguments into a command and are decoded in
//! place.

use std::sync::Arc;

use shared::protocol::render_cmd::{
    GLCmd, MAX_WEBGL_SHADER_SOURCE_BYTES, ShaderType, TexImage3DSource,
    webgl_upload_is_within_limit,
};

use frame_wire::gl_resource::*;

use crate::codes;
use crate::validate::{GlDecodeContext, TransformFeedbackPhase};

const GL_VERTEX_SHADER: u32 = 0x8B31;
const GL_FRAGMENT_SHADER: u32 = 0x8B30;

/// The separator `transformFeedbackVaryings` joins its names with, in both the
/// op's string argument and a record's payload.
pub const VARYINGS_SEPARATOR: char = '\u{1F}';

/// A call's bytes, wherever they are.
///
/// The op has them as a slice V8 lent it; the decoder has them as the words of a
/// record. Both are copied exactly once, into the vector the command owns --
/// converting the words to a byte slice first would be a second copy of a
/// texture on the render path.
#[derive(Clone, Copy, Debug)]
pub enum Payload<'a> {
    Bytes(&'a [u8]),
    /// `len` bytes, little-endian, in `words` (padded to a word).
    Words {
        words: &'a [u32],
        len: usize,
    },
}

impl Payload<'_> {
    pub fn len(&self) -> usize {
        match self {
            Payload::Bytes(bytes) => bytes.len(),
            Payload::Words { len, .. } => *len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The bytes, owned, or `None` if the allocation failed.
    fn to_vec(self) -> Option<Vec<u8>> {
        let mut owned = Vec::new();
        owned.try_reserve_exact(self.len()).ok()?;
        match self {
            Payload::Bytes(bytes) => owned.extend_from_slice(bytes),
            Payload::Words { words, len } => {
                let whole = len / 4;
                for word in &words[..whole] {
                    owned.extend_from_slice(&word.to_le_bytes());
                }
                if len % 4 != 0 {
                    owned.extend_from_slice(&words[whole].to_le_bytes()[..len % 4]);
                }
            }
        }
        Some(owned)
    }
}

/// Bytes for an upload, held to the upload ceiling: `OUT_OF_MEMORY` and nothing
/// when they are over it or cannot be allocated.
pub fn bounded_upload<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    payload: Payload<'_>,
) -> Option<Vec<u8>> {
    let owned = if webgl_upload_is_within_limit(payload.len()) {
        payload.to_vec()
    } else {
        None
    };
    if owned.is_none() {
        context.push_error(canvas_id, codes::OUT_OF_MEMORY);
    }
    owned
}

/// Text for a call that keeps it, held to the shader-source ceiling.
fn bounded_text<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    payload: Payload<'_>,
) -> Option<String> {
    if payload.len() > MAX_WEBGL_SHADER_SOURCE_BYTES {
        context.push_error(canvas_id, codes::OUT_OF_MEMORY);
        return None;
    }
    let Some(bytes) = payload.to_vec() else {
        context.push_error(canvas_id, codes::OUT_OF_MEMORY);
        return None;
    };
    // UTF-8 already: the op's comes from a JavaScript string, and a record's was
    // checked by the envelope. A failure here would be a bug in one of those,
    // reported the way an unusable argument is rather than trusted.
    match String::from_utf8(bytes) {
        Ok(text) => Some(text),
        Err(_) => {
            context.push_error(canvas_id, codes::INVALID_VALUE);
            None
        }
    }
}

/// Text for a call the op takes as a plain string, with no ceiling of its own.
/// Only the envelope's bound applies.
fn unbounded_text(payload: Payload<'_>) -> Option<String> {
    String::from_utf8(payload.to_vec()?).ok()
}

/// `createShader`. An unknown type builds nothing and reports nothing -- what
/// the op has always done, and the facade checks the type before it gets here.
pub fn create_shader(canvas_id: u32, client_id: u32, ty: u32) -> Option<GLCmd> {
    let shader_type = match ty {
        GL_VERTEX_SHADER => ShaderType::Vertex,
        GL_FRAGMENT_SHADER => ShaderType::Fragment,
        _ => return None,
    };
    Some(GLCmd::CreateShader {
        canvas_id,
        client_id,
        shader_type,
    })
}

pub fn shader_source<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    shader_id: u32,
    source: Payload<'_>,
) -> Option<GLCmd> {
    Some(GLCmd::ShaderSource {
        shader_id,
        source: bounded_text(context, canvas_id, source)?,
        resp: None,
    })
}

pub fn buffer_data<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    target: u32,
    size: i32,
    data: Option<Payload<'_>>,
    usage: u32,
) -> Option<GLCmd> {
    let (size, data) = match data {
        Some(bytes) => {
            let owned = bounded_upload(context, canvas_id, bytes)?;
            // The payload is the authority when there is one: the render thread
            // uploads `data` and ignores `size`, so a caller-supplied `size` that
            // disagrees is a second answer to one question waiting for a reader
            // who trusts the wrong field. `02_webgl_context.js` passes `size = -1`
            // on this path precisely because the field is unused; the
            // negative-size check below must not run here, or every
            // `bufferData(target, ArrayBuffer, usage)` -- the common upload -- is
            // dropped with a spurious INVALID_VALUE (which shipped in v0.9.5).
            let len = i32::try_from(owned.len()).unwrap_or(i32::MAX);
            (len, Some(owned))
        }
        None => {
            // A negative size is INVALID_VALUE and a no-op; zero is a legal
            // request for an empty buffer. Both once left via a log line, so
            // `getError()` said NO_ERROR after a misuse.
            if size < 0 {
                context.push_error(canvas_id, codes::INVALID_VALUE);
                return None;
            }
            let requested = usize::try_from(size).ok()?;
            if !webgl_upload_is_within_limit(requested) {
                context.push_error(canvas_id, codes::OUT_OF_MEMORY);
                return None;
            }
            (size, None)
        }
    };
    Some(GLCmd::BufferData {
        canvas_id,
        target,
        size,
        data,
        usage,
    })
}

pub fn buffer_sub_data<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    target: u32,
    offset: i32,
    data: Payload<'_>,
) -> Option<GLCmd> {
    // WebGL 1.0 §5.14.5: a negative offset is INVALID_VALUE and the call a no-op.
    if offset < 0 {
        context.push_error(canvas_id, codes::INVALID_VALUE);
        return None;
    }
    Some(GLCmd::BufferSubData {
        canvas_id,
        target,
        offset,
        data: bounded_upload(context, canvas_id, data)?,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn tex_image_2d<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    target: u32,
    level: i32,
    internalformat: i32,
    width: i32,
    height: i32,
    border: i32,
    format: u32,
    type_: u32,
    data: Option<Payload<'_>>,
) -> Option<GLCmd> {
    let data = match data {
        Some(bytes) => Some(Arc::new(bounded_upload(context, canvas_id, bytes)?)),
        None => None,
    };
    Some(GLCmd::TexImage2D {
        canvas_id,
        target,
        level,
        internalformat,
        width,
        height,
        border,
        format,
        type_,
        data,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn tex_sub_image_2d<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    target: u32,
    level: i32,
    xoffset: i32,
    yoffset: i32,
    width: i32,
    height: i32,
    format: u32,
    type_: u32,
    data: Payload<'_>,
) -> Option<GLCmd> {
    Some(GLCmd::TexSubImage2D {
        canvas_id,
        target,
        level,
        xoffset,
        yoffset,
        width,
        height,
        format,
        type_,
        data: Arc::new(bounded_upload(context, canvas_id, data)?),
    })
}

#[allow(clippy::too_many_arguments)]
pub fn compressed_tex_image_2d<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    target: u32,
    level: i32,
    internalformat: u32,
    width: i32,
    height: i32,
    border: i32,
    data: Payload<'_>,
) -> Option<GLCmd> {
    Some(GLCmd::CompressedTexImage2D {
        canvas_id,
        target,
        level,
        internalformat,
        width,
        height,
        border,
        data: bounded_upload(context, canvas_id, data)?,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn compressed_tex_sub_image_2d<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    target: u32,
    level: i32,
    xoffset: i32,
    yoffset: i32,
    width: i32,
    height: i32,
    format: u32,
    data: Payload<'_>,
) -> Option<GLCmd> {
    Some(GLCmd::CompressedTexSubImage2D {
        canvas_id,
        target,
        level,
        xoffset,
        yoffset,
        width,
        height,
        format,
        data: bounded_upload(context, canvas_id, data)?,
    })
}

/// A 3D upload's source: a pixel-unpack buffer offset wins over pixels, and no
/// pixels reserves storage. The pixels are already the bytes from the caller's
/// `srcOffset` on. Over the ceiling, or unallocatable, is `OUT_OF_MEMORY`.
pub fn tex_3d_source<C: GlDecodeContext>(
    context: &mut C,
    canvas_id: u32,
    pixels: Option<Payload<'_>>,
    pbo_offset: Option<u32>,
) -> Option<TexImage3DSource> {
    if let Some(offset) = pbo_offset {
        return Some(TexImage3DSource::BufferOffset(offset));
    }
    match pixels {
        None => Some(TexImage3DSource::None),
        Some(bytes) => Some(TexImage3DSource::Bytes(Arc::new(bounded_upload(
            context, canvas_id, bytes,
        )?))),
    }
}

/// `transformFeedbackVaryings`, whose names arrive joined by U+001F. The vector
/// is sized to the names before any is copied, so what it holds is exactly them.
pub fn transform_feedback_varyings(
    canvas_id: u32,
    program: u32,
    joined: &str,
    buffer_mode: u32,
) -> GLCmd {
    let varyings = if joined.is_empty() {
        Vec::new()
    } else {
        let mut names = Vec::with_capacity(joined.matches(VARYINGS_SEPARATOR).count() + 1);
        names.extend(joined.split(VARYINGS_SEPARATOR).map(str::to_owned));
        names
    };
    GLCmd::TransformFeedbackVaryings {
        canvas_id,
        program,
        varyings,
        buffer_mode,
    }
}

/// The four transform-feedback transitions, and the state each leaves.
pub fn transform_feedback_transition<C: GlDecodeContext>(
    context: &mut C,
    opcode: u32,
    canvas_id: u32,
    primitive_mode: u32,
) -> GLCmd {
    let (phase, command) = match opcode {
        OPR_BEGIN_TRANSFORM_FEEDBACK => (
            TransformFeedbackPhase::Active,
            GLCmd::BeginTransformFeedback {
                canvas_id,
                primitive_mode,
            },
        ),
        OPR_PAUSE_TRANSFORM_FEEDBACK => (
            TransformFeedbackPhase::Paused,
            GLCmd::PauseTransformFeedback { canvas_id },
        ),
        OPR_RESUME_TRANSFORM_FEEDBACK => (
            TransformFeedbackPhase::Active,
            GLCmd::ResumeTransformFeedback { canvas_id },
        ),
        _ => (
            TransformFeedbackPhase::Inactive,
            GLCmd::EndTransformFeedback { canvas_id },
        ),
    };
    context.set_transform_feedback(canvas_id, phase);
    command
}

/// Decode one record of this block. Pass 1 has checked its shape: the word
/// counts, the payload lengths and padding, the bools, and that text is UTF-8.
#[allow(clippy::too_many_lines)]
pub(crate) fn decode_record<C: GlDecodeContext>(
    context: &mut C,
    opcode: u32,
    record: &[u32],
) -> Option<GLCmd> {
    #[inline]
    fn i(word: u32) -> i32 {
        word as i32
    }
    // A negative id is "none", as the ops read theirs.
    #[inline]
    fn signed_id(word: u32) -> Option<u32> {
        if (word as i32) < 0 { None } else { Some(word) }
    }
    /// The payload after a prefix of `prefix` words.
    #[inline]
    fn payload(record: &[u32], prefix: usize) -> Payload<'_> {
        Payload::Words {
            words: &record[prefix + 1..],
            len: record[prefix] as usize,
        }
    }

    let c = record[1];
    Some(match opcode {
        OPR_CREATE_BUFFER => GLCmd::CreateBuffer {
            canvas_id: c,
            client_id: record[2],
        },
        OPR_CREATE_FRAMEBUFFER => GLCmd::CreateFramebuffer {
            canvas_id: c,
            client_id: record[2],
        },
        OPR_CREATE_PROGRAM => GLCmd::CreateProgram {
            canvas_id: c,
            client_id: record[2],
        },
        OPR_CREATE_QUERY => GLCmd::CreateQuery {
            canvas_id: c,
            client_id: record[2],
        },
        OPR_CREATE_RENDERBUFFER => GLCmd::CreateRenderbuffer {
            canvas_id: c,
            client_id: record[2],
        },
        OPR_CREATE_SAMPLER => GLCmd::CreateSampler {
            canvas_id: c,
            client_id: record[2],
        },
        OPR_CREATE_SHADER => return create_shader(c, record[2], record[3]),
        OPR_CREATE_TEXTURE => GLCmd::CreateTexture {
            canvas_id: c,
            client_id: record[2],
        },
        OPR_CREATE_TRANSFORM_FEEDBACK => GLCmd::CreateTransformFeedback {
            canvas_id: c,
            client_id: record[2],
        },
        OPR_CREATE_VERTEX_ARRAY => GLCmd::CreateVertexArray {
            canvas_id: c,
            client_id: record[2],
        },
        OPR_DELETE_BUFFER => GLCmd::DeleteBuffer { buffer_id: c },
        OPR_DELETE_FRAMEBUFFER => GLCmd::DeleteFramebuffer { framebuffer_id: c },
        OPR_DELETE_PROGRAM => GLCmd::DeleteProgram { program_id: c },
        OPR_DELETE_QUERY => GLCmd::DeleteQuery { query: c },
        OPR_DELETE_RENDERBUFFER => GLCmd::DeleteRenderbuffer { renderbuffer_id: c },
        OPR_DELETE_SAMPLER => GLCmd::DeleteSampler { sampler: c },
        OPR_DELETE_SHADER => GLCmd::DeleteShader { shader_id: c },
        OPR_DELETE_SYNC => GLCmd::DeleteSync { sync: c },
        OPR_DELETE_TEXTURE => GLCmd::DeleteTexture { texture_id: c },
        OPR_DELETE_TRANSFORM_FEEDBACK => GLCmd::DeleteTransformFeedback { tf: c },
        OPR_DELETE_VERTEX_ARRAY => GLCmd::DeleteVertexArray { vao: c },
        OPR_ATTACH_SHADER => GLCmd::AttachShader {
            program_id: c,
            shader_id: record[2],
            resp: None,
        },
        OPR_COMPILE_SHADER => GLCmd::CompileShader { shader_id: c },
        OPR_LINK_PROGRAM => GLCmd::LinkProgram { program_id: c },
        OPR_BEGIN_QUERY => GLCmd::BeginQuery {
            canvas_id: c,
            target: record[2],
            query: record[3],
        },
        OPR_END_QUERY => GLCmd::EndQuery {
            canvas_id: c,
            target: record[2],
        },
        OPR_BEGIN_TRANSFORM_FEEDBACK => {
            transform_feedback_transition(context, opcode, c, record[2])
        }
        OPR_END_TRANSFORM_FEEDBACK
        | OPR_PAUSE_TRANSFORM_FEEDBACK
        | OPR_RESUME_TRANSFORM_FEEDBACK => transform_feedback_transition(context, opcode, c, 0),
        OPR_BIND_TRANSFORM_FEEDBACK => GLCmd::BindTransformFeedback {
            canvas_id: c,
            target: record[2],
            tf: if record[3] == 0 {
                None
            } else {
                Some(record[3])
            },
        },
        OPR_BLIT_FRAMEBUFFER => GLCmd::BlitFramebuffer {
            canvas_id: c,
            src_x0: i(record[2]),
            src_y0: i(record[3]),
            src_x1: i(record[4]),
            src_y1: i(record[5]),
            dst_x0: i(record[6]),
            dst_y0: i(record[7]),
            dst_x1: i(record[8]),
            dst_y1: i(record[9]),
            mask: record[10],
            filter: record[11],
        },
        OPR_FENCE_SYNC => GLCmd::FenceSync {
            canvas_id: c,
            client_id: record[2],
            condition: record[3],
            flags: record[4],
        },
        OPR_FRAMEBUFFER_RENDERBUFFER => GLCmd::FramebufferRenderbuffer {
            canvas_id: c,
            target: record[2],
            attachment: record[3],
            renderbuffertarget: record[4],
            renderbuffer: signed_id(record[5]),
        },
        OPR_FRAMEBUFFER_TEXTURE_2D => GLCmd::FramebufferTexture2D {
            canvas_id: c,
            target: record[2],
            attachment: record[3],
            textarget: record[4],
            texture: signed_id(record[5]),
            level: i(record[6]),
        },
        OPR_RENDERBUFFER_STORAGE => GLCmd::RenderbufferStorage {
            canvas_id: c,
            target: record[2],
            internalformat: record[3],
            width: i(record[4]),
            height: i(record[5]),
        },
        OPR_RENDERBUFFER_STORAGE_MULTISAMPLE => GLCmd::RenderbufferStorageMultisample {
            canvas_id: c,
            target: record[2],
            samples: i(record[3]),
            internal_format: record[4],
            width: i(record[5]),
            height: i(record[6]),
        },
        OPR_TEX_STORAGE_2D => GLCmd::TexStorage2D {
            canvas_id: c,
            target: record[2],
            levels: i(record[3]),
            internal_format: record[4],
            width: i(record[5]),
            height: i(record[6]),
        },
        OPR_TEX_STORAGE_3D => GLCmd::TexStorage3D {
            canvas_id: c,
            target: record[2],
            levels: i(record[3]),
            internal_format: record[4],
            width: i(record[5]),
            height: i(record[6]),
            depth: i(record[7]),
        },
        OPR_UNIFORM_BLOCK_BINDING => GLCmd::UniformBlockBinding {
            program_id: c,
            uniform_block_index: record[2],
            uniform_block_binding: record[3],
        },
        OPR_LOSE_CONTEXT => GLCmd::DebugLoseContext { canvas_id: c },
        OPR_TEX_IMAGE_2D_FROM_IMAGE => {
            return context.image_upload(crate::validate::ImageUpload::Full {
                canvas_id: c,
                target: record[2],
                level: i(record[3]),
                internalformat: i(record[4]),
                format: record[5],
                type_: record[6],
                image_id: record[7],
            });
        }
        OPR_TEX_SUB_IMAGE_2D_FROM_IMAGE => {
            return context.image_upload(crate::validate::ImageUpload::Sub {
                canvas_id: c,
                target: record[2],
                level: i(record[3]),
                xoffset: i(record[4]),
                yoffset: i(record[5]),
                format: record[6],
                type_: record[7],
                image_id: record[8],
            });
        }

        OPR_SHADER_SOURCE => return shader_source(context, c, record[2], payload(record, 3)),
        OPR_BIND_ATTRIB_LOCATION => GLCmd::BindAttribLocation {
            program_id: c,
            index: record[2],
            name: unbounded_text(payload(record, 3))?,
        },
        OPR_BUFFER_DATA => {
            let data = (record[5] != 0).then(|| payload(record, 6));
            return buffer_data(context, c, record[2], i(record[3]), data, record[4]);
        }
        OPR_BUFFER_SUB_DATA => {
            return buffer_sub_data(context, c, record[2], i(record[3]), payload(record, 4));
        }
        OPR_TEX_IMAGE_2D => {
            let data = (record[10] != 0).then(|| payload(record, 11));
            return tex_image_2d(
                context,
                c,
                record[2],
                i(record[3]),
                i(record[4]),
                i(record[5]),
                i(record[6]),
                i(record[7]),
                record[8],
                record[9],
                data,
            );
        }
        OPR_TEX_SUB_IMAGE_2D => {
            return tex_sub_image_2d(
                context,
                c,
                record[2],
                i(record[3]),
                i(record[4]),
                i(record[5]),
                i(record[6]),
                i(record[7]),
                record[8],
                record[9],
                payload(record, 10),
            );
        }
        OPR_COMPRESSED_TEX_IMAGE_2D => {
            return compressed_tex_image_2d(
                context,
                c,
                record[2],
                i(record[3]),
                record[4],
                i(record[5]),
                i(record[6]),
                i(record[7]),
                payload(record, 8),
            );
        }
        OPR_COMPRESSED_TEX_SUB_IMAGE_2D => {
            return compressed_tex_sub_image_2d(
                context,
                c,
                record[2],
                i(record[3]),
                i(record[4]),
                i(record[5]),
                i(record[6]),
                i(record[7]),
                record[8],
                payload(record, 9),
            );
        }
        OPR_TEX_IMAGE_3D => {
            let pbo = (i(record[11]) >= 0).then_some(record[11]);
            let pixels = (record[12] != 0).then(|| payload(record, 13));
            GLCmd::TexImage3D {
                canvas_id: c,
                target: record[2],
                level: i(record[3]),
                internal_format: i(record[4]),
                width: i(record[5]),
                height: i(record[6]),
                depth: i(record[7]),
                border: i(record[8]),
                format: record[9],
                ty: record[10],
                data: tex_3d_source(context, c, pixels, pbo)?,
            }
        }
        OPR_TEX_SUB_IMAGE_3D => {
            let pbo = (i(record[12]) >= 0).then_some(record[12]);
            let pixels = (record[13] != 0).then(|| payload(record, 14));
            GLCmd::TexSubImage3D {
                canvas_id: c,
                target: record[2],
                level: i(record[3]),
                xoffset: i(record[4]),
                yoffset: i(record[5]),
                zoffset: i(record[6]),
                width: i(record[7]),
                height: i(record[8]),
                depth: i(record[9]),
                format: record[10],
                ty: record[11],
                data: tex_3d_source(context, c, pixels, pbo)?,
            }
        }
        OPR_DRAW_BUFFERS => GLCmd::DrawBuffers {
            canvas_id: c,
            buffers: record[3..].to_vec(),
        },
        OPR_INVALIDATE_FRAMEBUFFER => GLCmd::InvalidateFramebuffer {
            canvas_id: c,
            target: record[2],
            attachments: record[4..].to_vec(),
        },
        // The uploads whose pixels the host already holds. The ops drop a
        // snapshot id of 0 and a zero-area canvas source before they queue
        // anything, so a record carrying one is a producer that did not, and
        // the command is built rather than second-guessed: the renderer makes
        // the same decision for both lanes.
        OPR_TEX_IMAGE_2D_FROM_SNAPSHOT => GLCmd::TexImage2DFromSnapshot {
            canvas_id: c,
            target: record[2],
            level: i(record[3]),
            internalformat: i(record[4]),
            format: record[5],
            type_: record[6],
            snapshot_id: record[7],
        },
        OPR_TEX_SUB_IMAGE_2D_FROM_SNAPSHOT => GLCmd::TexSubImage2DFromSnapshot {
            canvas_id: c,
            target: record[2],
            level: i(record[3]),
            xoffset: i(record[4]),
            yoffset: i(record[5]),
            format: record[6],
            type_: record[7],
            snapshot_id: record[8],
        },
        OPR_TEX_IMAGE_2D_FROM_CANVAS2D => GLCmd::TexImage2DFromCanvas2D {
            canvas_id: c,
            target: record[2],
            level: i(record[3]),
            internalformat: i(record[4]),
            canvas_2d_id: record[5],
            x: i(record[6]),
            y: i(record[7]),
            width: record[8],
            height: record[9],
        },
        OPR_TEX_SUB_IMAGE_2D_FROM_CANVAS2D => GLCmd::TexSubImage2DFromCanvas2D {
            canvas_id: c,
            target: record[2],
            level: i(record[3]),
            xoffset: i(record[4]),
            yoffset: i(record[5]),
            canvas_2d_id: record[6],
            x: i(record[7]),
            y: i(record[8]),
            width: record[9],
            height: record[10],
        },

        OPR_TRANSFORM_FEEDBACK_VARYINGS => transform_feedback_varyings(
            c,
            record[2],
            &unbounded_text(payload(record, 4))?,
            record[3],
        ),
        _ => {
            debug_assert!(false, "resource opcode {opcode} slipped through pass 1");
            return None;
        }
    })
}
