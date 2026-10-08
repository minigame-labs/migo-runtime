import {
    op_viewport,
    op_clear_color,
    op_gl_flush,
    op_gl_is_context_lost,
    op_gl_lose_context,
    op_create_program,
    op_link_program,
    op_get_program_parameter,
    op_get_program_info_log,
    op_delete_program,
    op_create_shader,
    op_shader_source,
    op_compile_shader,
    op_attach_shader,
    op_detach_shader,
    op_validate_program,
    op_get_shader_parameter,
    op_get_shader_info_log,
    op_delete_shader,
    op_draw_arrays,
    op_draw_elements,
    op_get_attrib_location,
    op_bind_attrib_location,
    op_get_active_attrib,
    op_get_active_uniform,
    op_enable_vertex_attrib_array,
    op_vertex_attrib_pointer,
    op_create_buffer,
    op_bind_buffer,
    op_buffer_data,
    op_get_uniform_location,
    op_uniform3f,
    op_uniform_matrix_3fv,
    op_alloc_gl_resource_id,
    op_webgl_get_error,
    op_webgl_record_error,
    op_webgl_record_out_of_memory,
    op_webgl_get_context_attributes,
    op_webgl_record_attributes,
    op_enable,
    op_disable,
    op_get_parameter,
    op_get_gl_state,
    op_create_texture,
    op_delete_texture,
    op_bind_texture,
    op_active_texture,
    op_tex_image_2d,
    op_tex_image_2d_from_text_cache,
    op_tex_sub_image_2d,
    op_pixel_storei,
    op_compressed_tex_image_2d,
    op_compressed_tex_sub_image_2d,
    op_compressed_tex_image_3d,
    op_compressed_tex_sub_image_3d,
    op_buffer_sub_data,
    op_disable_vertex_attrib_array,
    op_clear_depth,
    op_clear_stencil,
    op_blend_func,
    op_blend_func_separate,
    op_blend_equation,
    op_blend_equation_separate,
    op_blend_color,
    op_depth_func,
    op_depth_mask,
    op_depth_range,
    op_stencil_func,
    op_stencil_func_separate,
    op_stencil_op,
    op_stencil_op_separate,
    op_stencil_mask,
    op_stencil_mask_separate,
    op_cull_face,
    op_front_face,
    op_color_mask,
    op_scissor,
    op_line_width,
    op_polygon_offset,
    op_uniform1i,
    op_uniform1f,
    op_uniform2f,
    op_uniform4f,
    op_uniform1iv,
    op_uniform1fv,
    op_uniform2iv,
    op_uniform2fv,
    op_uniform3iv,
    op_uniform3fv,
    op_uniform4iv,
    op_uniform4fv,
    op_uniform_matrix_2fv,
    op_uniform_matrix_4fv,
    op_uniform1uiv,
    op_uniform2uiv,
    op_uniform3uiv,
    op_uniform4uiv,
    op_uniform_matrix_2x3fv,
    op_uniform_matrix_2x4fv,
    op_uniform_matrix_3x2fv,
    op_uniform_matrix_3x4fv,
    op_uniform_matrix_4x2fv,
    op_uniform_matrix_4x3fv,
    op_create_framebuffer,
    op_delete_framebuffer,
    op_bind_framebuffer,
    op_framebuffer_texture_2d,
    op_framebuffer_texture_layer,
    op_framebuffer_renderbuffer,
    op_check_framebuffer_status,
    op_create_renderbuffer,
    op_delete_renderbuffer,
    op_delete_buffer,
    op_bind_renderbuffer,
    op_renderbuffer_storage,
    op_read_pixels,
    op_read_pixels_to_buffer,
    op_get_buffer_sub_data,
    op_hint,

    // WebGL 2.0 additions
    op_create_vertex_array,
    op_delete_vertex_array,
    op_vertex_attrib_divisor,
    op_vertex_attrib_4f,
    op_vertex_attrib_i4i,
    op_vertex_attrib_i4ui,
    op_vertex_attrib_i_pointer,
    op_draw_arrays_instanced,
    op_draw_elements_instanced,
    op_get_uniform_block_index,
    op_uniform_block_binding,
    op_bind_buffer_base,
    op_bind_buffer_range,
    op_tex_storage_2d,
    op_blit_framebuffer,
    op_invalidate_framebuffer,
    op_invalidate_sub_framebuffer,
    op_renderbuffer_storage_multisample,
    op_create_sampler,
    op_delete_sampler,
    op_bind_sampler,
    op_sampler_parameteri,
    op_sampler_parameterf,
    op_fence_sync,
    op_wait_sync,
    op_delete_sync,
    op_client_wait_sync,
    op_draw_buffers,
    op_alloc_gl_resource_id as op_alloc_gl_resource_id_webgl2,
    op_webgl_query_gpu_caps,
    op_create_query,
    op_delete_query,
    op_begin_query,
    op_end_query,
    op_get_query_parameter,
    op_create_transform_feedback,
    op_delete_transform_feedback,
    op_bind_transform_feedback,
    op_begin_transform_feedback,
    op_end_transform_feedback,
    op_pause_transform_feedback,
    op_resume_transform_feedback,
    op_transform_feedback_varyings,
    op_get_transform_feedback_varying,
    op_tex_image_3d,
    op_tex_sub_image_3d,
    op_tex_storage_3d,
} from "ext:core/ops";

import { core, primordials } from "ext:core/mod.js";

const { isArrayBuffer, isTypedArray, isDataView, isSharedArrayBuffer } = core;

const {
    ArrayBufferIsView,
    ArrayIsArray,
    TypedArrayPrototypeGetBuffer,
    TypedArrayPrototypeGetByteLength,
    TypedArrayPrototypeGetByteOffset,
    TypedArrayPrototypeGetLength,
    TypedArrayPrototypeGetSymbolToStringTag,
    TypedArrayPrototypeSet,
    Uint8Array,
    Uint32Array,
    Int32Array,
    Float32Array,
    DataViewPrototypeGetBuffer,
    DataViewPrototypeGetByteLength,
    DataViewPrototypeGetByteOffset,
    ArrayBufferPrototypeGetByteLength,
    MathMax,
    MathMin,
    MathTrunc,
    MathRound,
    MathCeil,
    PromisePrototypeThen,
    PromiseResolve,
    MathFround,
    NumberIsFinite,
    NumberIsInteger,
    ReflectApply,
    StringPrototypeCharCodeAt,
    StringPrototypeStartsWith,
    StringPrototypeToLowerCase,
} = primordials;

import { WebglConstants } from "./01_constants.js";
import { setTimeout } from "ext:host_v8_web/02_timers.js";
import {
    flushRenderCommandStream,
    uploadTexImageSource,
    TEX_SOURCE_CALL_IMAGE_2D,
    TEX_SOURCE_CALL_SUB_IMAGE_2D,
    TEX_SOURCE_CALL_IMAGE_3D,
    TEX_SOURCE_CALL_SUB_IMAGE_3D,
    TEX_SOURCE_IMAGE,
    TEX_SOURCE_CANVAS,
    TEX_SOURCE_SNAPSHOT,
    TEX_SOURCE_PIXELS,
    encodeViewport,
    encodeClear,
    encodeClearColor,
    encodeClearDepth,
    encodeClearStencil,
    encodeEnable,
    encodeDisable,
    encodeUseProgram,
    encodeBindBuffer,
    encodeBindTexture,
    encodeActiveTexture,
    encodeBindFramebuffer,
    encodeBindRenderbuffer,
    encodeBindVertexArray,
    encodeBindSampler,
    encodeEnableVertexAttribArray,
    encodeDisableVertexAttribArray,
    encodeVertexAttribPointer,
    encodeVertexAttribDivisor,
    encodeVertexAttrib4f,
    encodeVertexAttribI4i,
    encodeVertexAttribI4ui,
    encodeVertexAttribIPointer,
    encodeClearBufferfv,
    encodeClearBufferiv,
    encodeClearBufferuiv,
    encodeClearBufferfi,
    encodeCopyTexImage2D,
    encodeCopyTexSubImage2D,
    encodeCopyTexSubImage3D,
    encodeCopyBufferSubData,
    encodeSampleCoverage,
    encodeFlush,
    encodeWebglContext,
    encodeBlendFunc,
    encodeBlendFuncSeparate,
    encodeBlendEquation,
    encodeBlendEquationSeparate,
    encodeBlendColor,
    encodeDepthFunc,
    encodeDepthMask,
    encodeDepthRange,
    encodeStencilFunc,
    encodeStencilFuncSeparate,
    encodeStencilOp,
    encodeStencilOpSeparate,
    encodeStencilMask,
    encodeStencilMaskSeparate,
    encodeCullFace,
    encodeFrontFace,
    encodeColorMask,
    encodeScissor,
    encodeLineWidth,
    encodePolygonOffset,
    encodeTexParameteri,
    encodeTexParameterf,
    encodeGenerateMipmap,
    encodePixelStorei,
    encodeHint,
    encodeSamplerParameteri,
    encodeSamplerParameterf,
    encodeDrawArrays,
    encodeDrawElements,
    encodeDrawArraysInstanced,
    encodeDrawElementsInstanced,
    encodeBindBufferBase,
    encodeBindBufferRange,
    encodeReadBuffer,
    encodeUniform1i,
    encodeUniform1f,
    encodeUniform2f,
    encodeUniform3f,
    encodeUniform4f,
    encodeUniform1iv,
    encodeUniform1fv,
    encodeUniform2iv,
    encodeUniform2fv,
    encodeUniform3iv,
    encodeUniform3fv,
    encodeUniform4iv,
    encodeUniform4fv,
    encodeUniformMatrix2fv,
    encodeUniformMatrix3fv,
    encodeUniformMatrix4fv,
    encodeUniformMatrix2x3fv,
    encodeUniformMatrix2x4fv,
    encodeUniformMatrix3x2fv,
    encodeUniformMatrix3x4fv,
    encodeUniformMatrix4x2fv,
    encodeUniformMatrix4x3fv,
    encodeUniform1uiv,
    encodeUniform2uiv,
    encodeUniform3uiv,
    encodeUniform4uiv,
} from "./00_render_command_stream.js";

// -- Ordered-raw op wrappers --
// Built once at module init. Each wrapper: flush pending stream, then dispatch.
// rest-args are ONLY on this raw path; the hot encode path has no rest allocations.
//
// Direct / no-submit ops (call without flush):
//   op_alloc_gl_resource_id, op_gl_is_context_lost, op_webgl_get_context_attributes,
//   op_webgl_record_attributes, op_webgl_query_gpu_caps.
//
// All others: orderedRaw(op) -> flushRenderCommandStream() then ReflectApply.

function _makeOrderedRaw(op) {
    // Capture op reference at init time so game code cannot replace it.
    return function orderedRawOp(...args) {
        flushRenderCommandStream();
        return ReflectApply(op, undefined, args);
    };
}

const _rawGlFlush            = _makeOrderedRaw(op_gl_flush);
const _rawClearColor         = _makeOrderedRaw(op_clear_color);
const _rawEnable             = _makeOrderedRaw(op_enable);
const _rawDisable            = _makeOrderedRaw(op_disable);
const _rawGlLoseContext      = _makeOrderedRaw(op_gl_lose_context);
const _rawCreateProgram      = _makeOrderedRaw(op_create_program);
const _rawLinkProgram        = _makeOrderedRaw(op_link_program);
const _rawGetProgramParameter= _makeOrderedRaw(op_get_program_parameter);
const _rawGetProgramInfoLog  = _makeOrderedRaw(op_get_program_info_log);
const _rawDeleteProgram      = _makeOrderedRaw(op_delete_program);
const _rawCreateShader       = _makeOrderedRaw(op_create_shader);
const _rawShaderSource       = _makeOrderedRaw(op_shader_source);
const _rawCompileShader      = _makeOrderedRaw(op_compile_shader);
const _rawAttachShader       = _makeOrderedRaw(op_attach_shader);
const _rawDetachShader       = _makeOrderedRaw(op_detach_shader);
const _rawValidateProgram    = _makeOrderedRaw(op_validate_program);
const _rawGetShaderParameter = _makeOrderedRaw(op_get_shader_parameter);
const _rawGetShaderInfoLog   = _makeOrderedRaw(op_get_shader_info_log);
const _rawDeleteShader       = _makeOrderedRaw(op_delete_shader);
const _rawDrawArrays         = _makeOrderedRaw(op_draw_arrays);
const _rawDrawElements       = _makeOrderedRaw(op_draw_elements);
const _rawGetAttribLocation  = _makeOrderedRaw(op_get_attrib_location);
const _rawBindAttribLocation = _makeOrderedRaw(op_bind_attrib_location);
const _rawGetActiveAttrib    = _makeOrderedRaw(op_get_active_attrib);
const _rawGetActiveUniform   = _makeOrderedRaw(op_get_active_uniform);
const _rawEnableVertexAttribArray = _makeOrderedRaw(op_enable_vertex_attrib_array);
const _rawCreateBuffer       = _makeOrderedRaw(op_create_buffer);
const _rawBindBuffer         = _makeOrderedRaw(op_bind_buffer);
const _rawBufferData         = _makeOrderedRaw(op_buffer_data);
const _rawGetUniformLocation = _makeOrderedRaw(op_get_uniform_location);
const _rawGetParameter       = _makeOrderedRaw(op_get_parameter);
const _rawGetGlState         = _makeOrderedRaw(op_get_gl_state);

// The numbers of `frame_wire::sync::gl_state`: which state query `op_get_gl_state` asks. Mirrored by hand, and
// checked against the Rust table by `render_stream_js_agreement`.
const GL_STATE_INTERNALFORMAT_SAMPLES = 1;
const GL_STATE_FRAMEBUFFER_ATTACHMENT_PARAMETER = 2;
const GL_STATE_ACTIVE_UNIFORM_BLOCK_NAME = 3;
const GL_STATE_ACTIVE_UNIFORM_BLOCK_PARAMETER = 4;
const GL_STATE_UNIFORM_INDICES = 5;
const GL_STATE_ACTIVE_UNIFORMS_PARAMETER = 6;
const GL_STATE_UNIFORM_VALUE = 7;
const GL_STATE_FRAG_DATA_LOCATION = 8;
const GL_STATE_LINK_RESULT = 9;

// --- Producer-side capability shadow ---------------------------------------
//
// `isEnabled` used to be a synchronous round trip: flush the pending stream,
// hand the question to the render thread, and block this thread until it
// answered. It is the only WebGL query whose answer this side already knows,
// because this side is where every `enable` and `disable` was issued.
//
// It is also the only one where crossing gives a *worse* answer. The GL context
// is shared: Skia's Ganesh backend toggles GL_STENCIL_TEST inside its own
// batches without going through the renderer's tracker (which is why
// graphics/src/backend/gl/state_tracker.rs always re-issues that one), and the
// engine itself enables and disables GL_SCISSOR_TEST around blits and damage
// regions. So the driver bit is not content's bit. WebGL defines `isEnabled`
// purely in terms of content's own calls plus the initial state, and that is
// exactly what this table holds.
//
// The renderer's `CapabilityShadow` is not the same thing and cannot serve
// here: it is a de-duplication cache of what the driver was last told, and
// `invalidate_after_external_gl_use` deliberately forgets all of it whenever
// Skia has touched the context.
//
// Order matches TOGGLEABLE_CAPS in graphics/src/canvas/manager/types.rs. Both
// lists are the same list, and scripts/test-webgl-capability-table-contract.sh
// fails if they stop being.
const _TOGGLEABLE_CAPS = [
    WebglConstants.BLEND,
    WebglConstants.CULL_FACE,
    WebglConstants.DEPTH_TEST,
    WebglConstants.DITHER,
    WebglConstants.POLYGON_OFFSET_FILL,
    WebglConstants.SAMPLE_ALPHA_TO_COVERAGE,
    WebglConstants.SAMPLE_COVERAGE,
    WebglConstants.SCISSOR_TEST,
    WebglConstants.STENCIL_TEST,
    WebglConstants.RASTERIZER_DISCARD,
];

// Capability enum -> its bit. A ten-entry Map, so an enum outside the set is
/// `undefined` rather than a bit that happens to be free: `enable()` with an
/// invalid enum is GL_INVALID_ENUM and must leave the state alone.
const _CAP_BIT = new Map();
for (let i = 0; i < _TOGGLEABLE_CAPS.length; i++) {
    _CAP_BIT.set(_TOGGLEABLE_CAPS[i], 1 << i);
}
// WebGL 2's only capability: WebGL 1 has no RASTERIZER_DISCARD.
const _RASTERIZER_DISCARD_BIT = _CAP_BIT.get(WebglConstants.RASTERIZER_DISCARD);

// GL ES initial state: every capability starts disabled except GL_DITHER.
// Spelled as a lookup rather than a literal so it cannot drift from the order
// above -- the bit for DITHER is wherever DITHER is in the list.
const _CAP_INITIAL = _CAP_BIT.get(WebglConstants.DITHER);

// Bumped whenever the render thread rebuilds the GL context. A context that
// comes back is at its initial state, so a shadow filled in before the loss
// describes a context that no longer exists.
//
// A generation counter rather than a registry of live contexts: the reset has
// to reach every context including offscreen ones, and a module-level list of
// them would keep them alive. Each context compares one integer and refills
// itself on first use after a loss.
let _capGeneration = 0;

function _bumpCapabilityGeneration() {
    _capGeneration++;
}
const _rawCreateTexture      = _makeOrderedRaw(op_create_texture);
const _rawDeleteTexture      = _makeOrderedRaw(op_delete_texture);
const _rawTexImage2D         = _makeOrderedRaw(op_tex_image_2d);
const _rawTexImage2DFromTextCache= _makeOrderedRaw(op_tex_image_2d_from_text_cache);
const _rawTexSubImage2D      = _makeOrderedRaw(op_tex_sub_image_2d);
const _rawPixelStorei        = _makeOrderedRaw(op_pixel_storei);
const _rawCompressedTexImage2D = _makeOrderedRaw(op_compressed_tex_image_2d);
const _rawCompressedTexSubImage2D= _makeOrderedRaw(op_compressed_tex_sub_image_2d);
const _rawCompressedTexImage3D= _makeOrderedRaw(op_compressed_tex_image_3d);
const _rawCompressedTexSubImage3D= _makeOrderedRaw(op_compressed_tex_sub_image_3d);
const _rawBufferSubData      = _makeOrderedRaw(op_buffer_sub_data);
const _rawDisableVertexAttribArray= _makeOrderedRaw(op_disable_vertex_attrib_array);
const _rawClearDepth         = _makeOrderedRaw(op_clear_depth);
const _rawClearStencil       = _makeOrderedRaw(op_clear_stencil);
const _rawBlendFunc          = _makeOrderedRaw(op_blend_func);
const _rawBlendFuncSeparate  = _makeOrderedRaw(op_blend_func_separate);
const _rawBlendEquation      = _makeOrderedRaw(op_blend_equation);
const _rawBlendEquationSeparate= _makeOrderedRaw(op_blend_equation_separate);
const _rawBlendColor         = _makeOrderedRaw(op_blend_color);
const _rawDepthFunc          = _makeOrderedRaw(op_depth_func);
const _rawDepthRange         = _makeOrderedRaw(op_depth_range);
const _rawStencilFunc        = _makeOrderedRaw(op_stencil_func);
const _rawStencilFuncSeparate= _makeOrderedRaw(op_stencil_func_separate);
const _rawStencilOp          = _makeOrderedRaw(op_stencil_op);
const _rawStencilOpSeparate  = _makeOrderedRaw(op_stencil_op_separate);
const _rawStencilMask        = _makeOrderedRaw(op_stencil_mask);
const _rawStencilMaskSeparate= _makeOrderedRaw(op_stencil_mask_separate);
const _rawCullFace           = _makeOrderedRaw(op_cull_face);
const _rawFrontFace          = _makeOrderedRaw(op_front_face);
const _rawScissor            = _makeOrderedRaw(op_scissor);
const _rawLineWidth          = _makeOrderedRaw(op_line_width);
const _rawPolygonOffset      = _makeOrderedRaw(op_polygon_offset);
const _rawUniform1iv         = _makeOrderedRaw(op_uniform1iv);
const _rawUniform1fv         = _makeOrderedRaw(op_uniform1fv);
const _rawUniform2iv         = _makeOrderedRaw(op_uniform2iv);
const _rawUniform2fv         = _makeOrderedRaw(op_uniform2fv);
const _rawUniform3iv         = _makeOrderedRaw(op_uniform3iv);
const _rawUniform3fv         = _makeOrderedRaw(op_uniform3fv);
const _rawUniform4iv         = _makeOrderedRaw(op_uniform4iv);
const _rawUniform4fv         = _makeOrderedRaw(op_uniform4fv);
const _rawUniform1uiv        = _makeOrderedRaw(op_uniform1uiv);
const _rawUniform2uiv        = _makeOrderedRaw(op_uniform2uiv);
const _rawUniform3uiv        = _makeOrderedRaw(op_uniform3uiv);
const _rawUniform4uiv        = _makeOrderedRaw(op_uniform4uiv);
const _rawUniformMatrix2x3fv  = _makeOrderedRaw(op_uniform_matrix_2x3fv);
const _rawUniformMatrix2x4fv  = _makeOrderedRaw(op_uniform_matrix_2x4fv);
const _rawUniformMatrix3x2fv  = _makeOrderedRaw(op_uniform_matrix_3x2fv);
const _rawUniformMatrix3x4fv  = _makeOrderedRaw(op_uniform_matrix_3x4fv);
const _rawUniformMatrix4x2fv  = _makeOrderedRaw(op_uniform_matrix_4x2fv);
const _rawUniformMatrix4x3fv  = _makeOrderedRaw(op_uniform_matrix_4x3fv);
const _rawHint               = _makeOrderedRaw(op_hint);
const _UNIFORM_UI_ENCODERS = [null, encodeUniform1uiv, encodeUniform2uiv, encodeUniform3uiv, encodeUniform4uiv];
const _UNIFORM_UI_RAW = [null, _rawUniform1uiv, _rawUniform2uiv, _rawUniform3uiv, _rawUniform4uiv];
const _rawReadPixels         = _makeOrderedRaw(op_read_pixels);
const _rawReadPixelsToBuffer = _makeOrderedRaw(op_read_pixels_to_buffer);
const _rawGetBufferSubData   = _makeOrderedRaw(op_get_buffer_sub_data);
const _rawCreateFramebuffer  = _makeOrderedRaw(op_create_framebuffer);
const _rawDeleteFramebuffer  = _makeOrderedRaw(op_delete_framebuffer);
const _rawBindFramebuffer    = _makeOrderedRaw(op_bind_framebuffer);
const _rawFramebufferTexture2D= _makeOrderedRaw(op_framebuffer_texture_2d);
const _rawFramebufferTextureLayer= _makeOrderedRaw(op_framebuffer_texture_layer);
const _rawFramebufferRenderbuffer= _makeOrderedRaw(op_framebuffer_renderbuffer);
const _rawCheckFramebufferStatus= _makeOrderedRaw(op_check_framebuffer_status);
const _rawCreateRenderbuffer = _makeOrderedRaw(op_create_renderbuffer);
const _rawDeleteRenderbuffer = _makeOrderedRaw(op_delete_renderbuffer);
const _rawDeleteBuffer       = _makeOrderedRaw(op_delete_buffer);
const _rawBindRenderbuffer   = _makeOrderedRaw(op_bind_renderbuffer);
const _rawRenderbufferStorage= _makeOrderedRaw(op_renderbuffer_storage);
// WebGL2 ordered raw ops
const _rawCreateVertexArray  = _makeOrderedRaw(op_create_vertex_array);
const _rawDeleteVertexArray  = _makeOrderedRaw(op_delete_vertex_array);
const _rawVertexAttribDivisor= _makeOrderedRaw(op_vertex_attrib_divisor);
const _rawVertexAttrib4f     = _makeOrderedRaw(op_vertex_attrib_4f);
const _rawVertexAttribI4i    = _makeOrderedRaw(op_vertex_attrib_i4i);
const _rawVertexAttribI4ui   = _makeOrderedRaw(op_vertex_attrib_i4ui);
const _rawVertexAttribIPointer= _makeOrderedRaw(op_vertex_attrib_i_pointer);
const _rawDrawArraysInstanced= _makeOrderedRaw(op_draw_arrays_instanced);
const _rawDrawElementsInstanced= _makeOrderedRaw(op_draw_elements_instanced);
const _rawGetUniformBlockIndex= _makeOrderedRaw(op_get_uniform_block_index);
const _rawUniformBlockBinding= _makeOrderedRaw(op_uniform_block_binding);
const _rawBindBufferBase     = _makeOrderedRaw(op_bind_buffer_base);
const _rawBindBufferRange    = _makeOrderedRaw(op_bind_buffer_range);
const _rawTexStorage2D       = _makeOrderedRaw(op_tex_storage_2d);
const _rawBlitFramebuffer    = _makeOrderedRaw(op_blit_framebuffer);
const _rawInvalidateFramebuffer= _makeOrderedRaw(op_invalidate_framebuffer);
const _rawInvalidateSubFramebuffer= _makeOrderedRaw(op_invalidate_sub_framebuffer);
const _rawRenderbufferStorageMultisample= _makeOrderedRaw(op_renderbuffer_storage_multisample);
const _rawCreateSampler      = _makeOrderedRaw(op_create_sampler);
const _rawDeleteSampler      = _makeOrderedRaw(op_delete_sampler);
const _rawBindSampler        = _makeOrderedRaw(op_bind_sampler);
const _rawSamplerParameteri  = _makeOrderedRaw(op_sampler_parameteri);
const _rawSamplerParameterf  = _makeOrderedRaw(op_sampler_parameterf);
const _rawFenceSync          = _makeOrderedRaw(op_fence_sync);
const _rawWaitSync           = _makeOrderedRaw(op_wait_sync);
const _rawDeleteSync         = _makeOrderedRaw(op_delete_sync);
const _rawClientWaitSync     = _makeOrderedRaw(op_client_wait_sync);
const _rawDrawBuffers        = _makeOrderedRaw(op_draw_buffers);
const _rawCreateQuery        = _makeOrderedRaw(op_create_query);
const _rawDeleteQuery        = _makeOrderedRaw(op_delete_query);
const _rawBeginQuery         = _makeOrderedRaw(op_begin_query);
const _rawEndQuery           = _makeOrderedRaw(op_end_query);
const _rawGetQueryParameter  = _makeOrderedRaw(op_get_query_parameter);
const _rawCreateTransformFeedback= _makeOrderedRaw(op_create_transform_feedback);
const _rawDeleteTransformFeedback= _makeOrderedRaw(op_delete_transform_feedback);
const _rawBindTransformFeedback= _makeOrderedRaw(op_bind_transform_feedback);
const _rawBeginTransformFeedback= _makeOrderedRaw(op_begin_transform_feedback);
const _rawEndTransformFeedback= _makeOrderedRaw(op_end_transform_feedback);
const _rawPauseTransformFeedback= _makeOrderedRaw(op_pause_transform_feedback);
const _rawResumeTransformFeedback= _makeOrderedRaw(op_resume_transform_feedback);
const _rawTransformFeedbackVaryings= _makeOrderedRaw(op_transform_feedback_varyings);
const _rawGetTransformFeedbackVarying= _makeOrderedRaw(op_get_transform_feedback_varying);

// The `{size, type, name}` introspection queries share one cache path, but the
// ops disagree about whether they take a canvas id -- this one does not, and
// that asymmetry is deliberate rather than an oversight to tidy away. Its
// renderer arm resolves the owning canvas from the program itself
// (`cm.programs[program].owner_canvas`) and makes that current, which is a
// stronger guarantee than trusting the id the caller passed. The difference is
// absorbed once here, at module scope, rather than by a closure built on every
// call.
const _fetchTransformFeedbackVarying = (_canvasId, programId, index) =>
    _rawGetTransformFeedbackVarying(programId, index);
const _rawTexImage3D         = _makeOrderedRaw(op_tex_image_3d);
const _rawTexSubImage3D      = _makeOrderedRaw(op_tex_sub_image_3d);
const _rawTexStorage3D       = _makeOrderedRaw(op_tex_storage_3d);

const GL_CURRENT_QUERY = 0x8865;
const GL_INVALID_ENUM = 0x0500;
const GL_INVALID_VALUE = 0x0501;
// COLOR_BUFFER_BIT | DEPTH_BUFFER_BIT | STENCIL_BUFFER_BIT: every buffer `clear` can name.
const CLEAR_BUFFER_BITS = 0x4000 | 0x0100 | 0x0400;
const GL_INVALID_OPERATION = 0x0502;
const MAX_WEBGL_UPLOAD_BYTES = 64 * 1024 * 1024;
const MAX_WEBGL_SHADER_SOURCE_CODE_UNITS = 1024 * 1024;
const MAX_WEBGL_GPU_2D_DIMENSION = 16384;
const MAX_WEBGL_GPU_2D_LEVELS = 15;            // log2(MAX_WEBGL_GPU_2D_DIMENSION) + 1
const MAX_WEBGL_GPU_3D_DIMENSION = 2048;
const MAX_WEBGL_GPU_ARRAY_LAYERS = 2048;
const MAX_WEBGL_GPU_SAMPLES = 16;

function recordGpuPreflightError(canvasId, code) {
    op_webgl_record_error(canvasId, code);
    return false;
}

function isKnownUnsizedFormat(format) {
    switch (format) {
        case 0x1902: // DEPTH_COMPONENT
        case 0x1903: // RED
        case 0x1906: // ALPHA
        case 0x1907: // RGB
        case 0x1908: // RGBA
        case 0x1909: // LUMINANCE
        case 0x190A: // LUMINANCE_ALPHA
        case 0x8227: // RG
        case 0x84F9: // DEPTH_STENCIL
        case 0x8D94: // RED_INTEGER
        case 0x8228: // RG_INTEGER
        case 0x8D98: // RGB_INTEGER
        case 0x8D99: // RGBA_INTEGER
            return true;
        default:
            return false;
    }
}

function isKnownPixelType(type) {
    switch (type) {
        case 0x1400: case 0x1401: case 0x1402: case 0x1403:
        case 0x1404: case 0x1405: case 0x1406: case 0x140B:
        case 0x8033: case 0x8034: case 0x8363: case 0x8368:
        case 0x84FA: case 0x8D61: case 0x8DAD:
            return true;
        default:
            return false;
    }
}

function maxMipLevels(dimension) {
    let levels = 0;
    for (let value = dimension; value > 0; value >>>= 1) levels++;
    return levels;
}

// ---- Uploads: which internal format, format and type go together, and how many bytes an upload reads ----------------
// ES 3.0 table 3.3: the unsized internal formats -- WebGL 1's only ones -- each uploaded from itself as the format, with
// these types.
const _UNSIZED_UPLOAD_TYPES = new Map([
    [0x1908, [0x1401, 0x8033, 0x8034]],     // RGBA: UNSIGNED_BYTE, UNSIGNED_SHORT_4_4_4_4, UNSIGNED_SHORT_5_5_5_1
    [0x1907, [0x1401, 0x8363]],             // RGB: UNSIGNED_BYTE, UNSIGNED_SHORT_5_6_5
    [0x190a, [0x1401]],                     // LUMINANCE_ALPHA
    [0x1909, [0x1401]],                     // LUMINANCE
    [0x1906, [0x1401]],                     // ALPHA
]);
// WebGL 1's types (it has no extension that adds one).
const _WEBGL1_UPLOAD_TYPES = [0x1401, 0x8363, 0x8033, 0x8034];
// ES 3.0 table 3.2: each sized internal format, the format it is uploaded from and the types it takes.
const _BYTE = 0x1400, _UBYTE = 0x1401, _SHORT = 0x1402, _USHORT = 0x1403, _INT = 0x1404, _UINT = 0x1405,
    _FLOAT = 0x1406, _HALF = 0x140b;
const _SIZED_UPLOADS = new Map([
    [0x8229, [0x1903, [_UBYTE]]],                       // R8: RED
    [0x8f94, [0x1903, [_BYTE]]],                        // R8_SNORM
    [0x822d, [0x1903, [_HALF, _FLOAT]]],                // R16F
    [0x822e, [0x1903, [_FLOAT]]],                       // R32F
    [0x8232, [0x8d94, [_UBYTE]]],                       // R8UI: RED_INTEGER
    [0x8231, [0x8d94, [_BYTE]]],                        // R8I
    [0x8234, [0x8d94, [_USHORT]]],                      // R16UI
    [0x8233, [0x8d94, [_SHORT]]],                       // R16I
    [0x8236, [0x8d94, [_UINT]]],                        // R32UI
    [0x8235, [0x8d94, [_INT]]],                         // R32I
    [0x822b, [0x8227, [_UBYTE]]],                       // RG8: RG
    [0x8f95, [0x8227, [_BYTE]]],                        // RG8_SNORM
    [0x822f, [0x8227, [_HALF, _FLOAT]]],                // RG16F
    [0x8230, [0x8227, [_FLOAT]]],                       // RG32F
    [0x8238, [0x8228, [_UBYTE]]],                       // RG8UI: RG_INTEGER
    [0x8237, [0x8228, [_BYTE]]],                        // RG8I
    [0x823a, [0x8228, [_USHORT]]],                      // RG16UI
    [0x8239, [0x8228, [_SHORT]]],                       // RG16I
    [0x823c, [0x8228, [_UINT]]],                        // RG32UI
    [0x823b, [0x8228, [_INT]]],                         // RG32I
    [0x8051, [0x1907, [_UBYTE]]],                       // RGB8: RGB
    [0x8c41, [0x1907, [_UBYTE]]],                       // SRGB8
    [0x8d62, [0x1907, [_UBYTE, 0x8363]]],               // RGB565: UNSIGNED_SHORT_5_6_5 too
    [0x8f96, [0x1907, [_BYTE]]],                        // RGB8_SNORM
    [0x8c3a, [0x1907, [0x8c3b, _HALF, _FLOAT]]],        // R11F_G11F_B10F: UNSIGNED_INT_10F_11F_11F_REV
    [0x8c3d, [0x1907, [0x8c3e, _HALF, _FLOAT]]],        // RGB9_E5: UNSIGNED_INT_5_9_9_9_REV
    [0x881b, [0x1907, [_HALF, _FLOAT]]],                // RGB16F
    [0x8815, [0x1907, [_FLOAT]]],                       // RGB32F
    [0x8d7d, [0x8d98, [_UBYTE]]],                       // RGB8UI: RGB_INTEGER
    [0x8d8f, [0x8d98, [_BYTE]]],                        // RGB8I
    [0x8d77, [0x8d98, [_USHORT]]],                      // RGB16UI
    [0x8d89, [0x8d98, [_SHORT]]],                       // RGB16I
    [0x8d71, [0x8d98, [_UINT]]],                        // RGB32UI
    [0x8d83, [0x8d98, [_INT]]],                         // RGB32I
    [0x8058, [0x1908, [_UBYTE]]],                       // RGBA8: RGBA
    [0x8c43, [0x1908, [_UBYTE]]],                       // SRGB8_ALPHA8
    [0x8f97, [0x1908, [_BYTE]]],                        // RGBA8_SNORM
    [0x8057, [0x1908, [_UBYTE, 0x8034, 0x8368]]],       // RGB5_A1: UNSIGNED_SHORT_5_5_5_1, UNSIGNED_INT_2_10_10_10_REV
    [0x8056, [0x1908, [_UBYTE, 0x8033]]],               // RGBA4: UNSIGNED_SHORT_4_4_4_4
    [0x8059, [0x1908, [0x8368]]],                       // RGB10_A2
    [0x881a, [0x1908, [_HALF, _FLOAT]]],                // RGBA16F
    [0x8814, [0x1908, [_FLOAT]]],                       // RGBA32F
    [0x8d7c, [0x8d99, [_UBYTE]]],                       // RGBA8UI: RGBA_INTEGER
    [0x8d8e, [0x8d99, [_BYTE]]],                        // RGBA8I
    [0x906f, [0x8d99, [0x8368]]],                       // RGB10_A2UI
    [0x8d76, [0x8d99, [_USHORT]]],                      // RGBA16UI
    [0x8d88, [0x8d99, [_SHORT]]],                       // RGBA16I
    [0x8d82, [0x8d99, [_INT]]],                         // RGBA32I
    [0x8d70, [0x8d99, [_UINT]]],                        // RGBA32UI
    [0x81a5, [0x1902, [_USHORT, _UINT]]],               // DEPTH_COMPONENT16: DEPTH_COMPONENT
    [0x81a6, [0x1902, [_UINT]]],                        // DEPTH_COMPONENT24
    [0x8cac, [0x1902, [_FLOAT]]],                       // DEPTH_COMPONENT32F
    [0x88f0, [0x84f9, [0x84fa]]],                       // DEPTH24_STENCIL8: DEPTH_STENCIL, UNSIGNED_INT_24_8
    [0x8cad, [0x84f9, [0x8dad]]],                       // DEPTH32F_STENCIL8: FLOAT_32_UNSIGNED_INT_24_8_REV
]);
// Every (format, type) an upload into an existing image may name in WebGL 2: the pairs of either table.
const _WEBGL2_SUB_UPLOADS = new Set();
for (const [format, types] of _UNSIZED_UPLOAD_TYPES) for (const type of types) _WEBGL2_SUB_UPLOADS.add(format * 0x10000 + type);
for (const [, [format, types]] of _SIZED_UPLOADS) for (const type of types) _WEBGL2_SUB_UPLOADS.add(format * 0x10000 + type);

// WebGL 2.0 3.7.6's table of uploads from a TexImageSource: the sized internal formats a source's 8-bit pixels convert
// to unambiguously -- unsigned integers of at most 8 bits, half floats, floats --, each with its format and types,
// besides the unsized ones of ES 3.0 table 3.3. The formats and types are those the table names.
const _SOURCE_UPLOADS = new Map([
    [0x8229, [0x1903, [_UBYTE]]],                   // R8: RED
    [0x822d, [0x1903, [_HALF, _FLOAT]]],            // R16F
    [0x822e, [0x1903, [_FLOAT]]],                   // R32F
    [0x8232, [0x8d94, [_UBYTE]]],                   // R8UI: RED_INTEGER
    [0x822b, [0x8227, [_UBYTE]]],                   // RG8: RG
    [0x822f, [0x8227, [_HALF, _FLOAT]]],            // RG16F
    [0x8230, [0x8227, [_FLOAT]]],                   // RG32F
    [0x8238, [0x8228, [_UBYTE]]],                   // RG8UI: RG_INTEGER
    [0x8051, [0x1907, [_UBYTE]]],                   // RGB8: RGB
    [0x8c41, [0x1907, [_UBYTE]]],                   // SRGB8
    [0x8d62, [0x1907, [_UBYTE, 0x8363]]],           // RGB565: UNSIGNED_SHORT_5_6_5 too
    [0x8c3a, [0x1907, [0x8c3b, _HALF, _FLOAT]]],    // R11F_G11F_B10F: UNSIGNED_INT_10F_11F_11F_REV too
    [0x8c3d, [0x1907, [_HALF, _FLOAT]]],            // RGB9_E5
    [0x881b, [0x1907, [_HALF, _FLOAT]]],            // RGB16F
    [0x8815, [0x1907, [_FLOAT]]],                   // RGB32F
    [0x8d7d, [0x8d98, [_UBYTE]]],                   // RGB8UI: RGB_INTEGER
    [0x8058, [0x1908, [_UBYTE]]],                   // RGBA8: RGBA
    [0x8c43, [0x1908, [_UBYTE]]],                   // SRGB8_ALPHA8
    [0x8057, [0x1908, [_UBYTE, 0x8034]]],           // RGB5_A1: UNSIGNED_SHORT_5_5_5_1 too
    [0x8059, [0x1908, [0x8368]]],                   // RGB10_A2: UNSIGNED_INT_2_10_10_10_REV
    [0x8056, [0x1908, [_UBYTE, 0x8033]]],           // RGBA4: UNSIGNED_SHORT_4_4_4_4 too
    [0x881a, [0x1908, [_HALF, _FLOAT]]],            // RGBA16F
    [0x8814, [0x1908, [_FLOAT]]],                   // RGBA32F
    [0x8d7c, [0x8d99, [_UBYTE]]],                   // RGBA8UI: RGBA_INTEGER
]);
const _SOURCE_FORMATS = [0x1908, 0x1907, 0x8227, 0x1903, 0x8d99, 0x8d98, 0x8228, 0x8d94, 0x1909, 0x1906, 0x190a];
const _SOURCE_TYPES = [_UBYTE, 0x8363, 0x8033, 0x8034, _HALF, _FLOAT, 0x8c3b, 0x8368];
const _SOURCE_PAIRS = new Set();
for (const [format, types] of _UNSIZED_UPLOAD_TYPES) for (const type of types) _SOURCE_PAIRS.add(format * 0x10000 + type);
for (const [, [format, types]] of _SOURCE_UPLOADS) for (const type of types) _SOURCE_PAIRS.add(format * 0x10000 + type);

// The view an upload of each type reads, and a read of it writes (WebGL 1.0 5.14.8 and 5.14.12, WebGL 2.0 3.7.6): its
// `Symbol.toStringTag` and bytes per element. UNSIGNED_BYTE also takes a Uint8ClampedArray. FLOAT_32_UNSIGNED_INT_24_8_REV
// has none: it is uploaded from a buffer or not at all. EXT_read_format_bgra's reversed shorts are only ever read.
const _UPLOAD_VIEWS = new Map([
    [_UBYTE, ["Uint8Array", 1]],
    [_BYTE, ["Int8Array", 1]],
    [_USHORT, ["Uint16Array", 2]],
    [_HALF, ["Uint16Array", 2]],
    [0x8d61, ["Uint16Array", 2]],          // HALF_FLOAT_OES (WebGL 1's OES_texture_half_float)
    [0x8363, ["Uint16Array", 2]],          // UNSIGNED_SHORT_5_6_5
    [0x8033, ["Uint16Array", 2]],          // UNSIGNED_SHORT_4_4_4_4
    [0x8034, ["Uint16Array", 2]],          // UNSIGNED_SHORT_5_5_5_1
    [0x8365, ["Uint16Array", 2]],          // UNSIGNED_SHORT_4_4_4_4_REV_EXT
    [0x8366, ["Uint16Array", 2]],          // UNSIGNED_SHORT_1_5_5_5_REV_EXT
    [_SHORT, ["Int16Array", 2]],
    [_UINT, ["Uint32Array", 4]],
    [0x8368, ["Uint32Array", 4]],          // UNSIGNED_INT_2_10_10_10_REV
    [0x8c3b, ["Uint32Array", 4]],          // UNSIGNED_INT_10F_11F_11F_REV
    [0x8c3e, ["Uint32Array", 4]],          // UNSIGNED_INT_5_9_9_9_REV
    [0x84fa, ["Uint32Array", 4]],          // UNSIGNED_INT_24_8
    [_INT, ["Int32Array", 4]],
    [_FLOAT, ["Float32Array", 4]],
]);

// Bytes per pixel of an upload's (format, type), the packed types whole; 0 for a pair the tables do not have.
function _uploadBytesPerPixel(format, type) {
    switch (type) {
        case 0x8363: case 0x8033: case 0x8034: return 2;
        case 0x8368: case 0x8c3b: case 0x8c3e: case 0x84fa: return 4;
        case 0x8dad: return 8;
        default: break;
    }
    const size = type === _UBYTE || type === _BYTE ? 1
        : type === _USHORT || type === _SHORT || type === _HALF || type === 0x8d61 ? 2
        : type === _UINT || type === _INT || type === _FLOAT ? 4 : 0;
    switch (format) {
        case 0x1908: case 0x8d99: case 0x8c42: return 4 * size;             // RGBA, RGBA_INTEGER, SRGB_ALPHA_EXT
        case 0x1907: case 0x8d98: case 0x8c40: return 3 * size;             // RGB, RGB_INTEGER, SRGB_EXT
        case 0x190a: case 0x8227: case 0x8228: return 2 * size;             // LUMINANCE_ALPHA, RG, RG_INTEGER
        case 0x1909: case 0x1906: case 0x1903: case 0x8d94: case 0x1902: return size;   // LUMINANCE, ALPHA, RED, RED_INTEGER, DEPTH_COMPONENT
        default: return 0;
    }
}

// The bytes an upload of `width` x `height` x `depth` pixels reads with the pixel-store state `store` (ES 3.0 3.7.2):
// rows padded to UNPACK_ALIGNMENT, UNPACK_ROW_LENGTH and (in 3D) UNPACK_IMAGE_HEIGHT in place of the size when set,
// the skips in front, the last row unpadded. The renderer refuses fewer bytes the same way
// (`unpack_convert::upload_bytes`); here it is the error content reads.
function _uploadBytes(store, width, height, depth, bpp, threeD) {
    if (width === 0 || height === 0 || depth === 0) return 0;
    const alignment = store.get(0x0cf5);
    const stride = MathCeil(((store.get(0x0cf2) | 0) || width) * bpp / alignment) * alignment;
    const image = stride * (threeD ? (store.get(0x806e) | 0) || height : height);
    const skipped = (threeD ? (store.get(0x806d) | 0) * image : 0)
        + (store.get(0x0cf3) | 0) * stride + (store.get(0x0cf4) | 0) * bpp;
    return skipped + (depth - 1) * image + (height - 1) * stride + width * bpp;
}

// ---- Texture images ---------------------------------------------------------------------------------------------------
// The context keeps what it knows of every image of every texture -- each level of a 2D, 3D or 2D-array texture and of
// each face of a cube map -- and judges by it, before anything is sent, what a browser judges by its own record: an
// upload into an image that is not there, past its edge, or from a format the image was not defined with; immutable
// storage defined again; mipmaps generated from a base they cannot be. An image is recorded when the call that defines
// it is sent, after every check of that call has passed, so the record is what the render side holds.
class TextureImage {
    // `format` and `type` are those the image's data was given in, which WebGL 1 holds every later upload to (WebGL 1.0,
    // "Texture Type in TexSubImage2D Calls"); a compressed image's are its internal format and 0.
    constructor(internalformat, format, type, width, height, depth, compressed) {
        this.internalformat = internalformat;
        this.format = format;
        this.type = type;
        this.width = width;
        this.height = height;
        this.depth = depth;
        this.compressed = compressed;
    }
}

// An image's key in its texture's record: the level, and the face of a cube map a face target names (0 for any other
// target). Both are the caller's to have checked.
function _imageKey(target, level) {
    return level * 8 + (target >= 0x8515 && target <= 0x851a ? target - 0x8515 : 0);
}

function _isPowerOfTwo(n) {
    return (n & (n - 1)) === 0;
}

// The record is changed only by these, at module scope: what is reachable from content -- every method of the context,
// underscored or not, is -- must not be able to put an image there that no call defined, or to do work its arguments
// size (WebGL's robustness bundle calls every method with hostile values).
function defineTextureImage(texture, target, level, image) {
    const images = texture._images || (texture._images = new Map());
    const key = _imageKey(Number(target) >>> 0, level | 0);
    const wasDepth = _isDepthImage(images.get(key));
    images.set(key, image);
    framebufferChanged();
    if (key === 0 && (Number(target) >>> 0) === 0x0de1 && _isDepthImage(image) !== wasDepth) {
        // TEXTURE_2D's level 0 turned a WEBGL_depth_texture image or stopped being one: the driver's filters follow
        // (`_driverFilter`). The texture is the one bound, as every call that defines an image has it.
        encodeTexParameteri(texture._ownerId, 0x0de1, 0x2801,
            _driverFilter(texture, 0x2801, _samplingParameter(texture._params, 0x2801)));
        encodeTexParameteri(texture._ownerId, 0x0de1, 0x2800,
            _driverFilter(texture, 0x2800, _samplingParameter(texture._params, 0x2800)));
    }
}

// WEBGL_depth_texture's images, WebGL 1's unsized DEPTH_COMPONENT and DEPTH_STENCIL (WebGL 2 has sized ones only).
function _isDepthImage(image) {
    return image != null && (image.internalformat === 0x1902 || image.internalformat === 0x84f9);
}

// The filter the driver is given for `value`, a filter (`pname`) of `texture`. WebGL 1 samples a WEBGL_depth_texture
// image through any filter, where ES 3.0 samples a depth texture whose filters are not NEAREST as incomplete when it
// does not compare: while TEXTURE_2D's level 0 is one, the driver is given NEAREST -- NEAREST_MIPMAP_NEAREST for a
// mipmap minification filter --, the filtering ES 3.0 has for depth without a comparison. What `getTexParameter`
// answers is what was set.
function _driverFilter(texture, pname, value) {
    if (!_isDepthImage(texture._images && texture._images.get(0))) return value;
    if (pname === 0x2800 || value === 0x2600 || value === 0x2601) return 0x2600;     // NEAREST
    return 0x2700;                                                                      // NEAREST_MIPMAP_NEAREST
}

function defineCompressedTextureImage(texture, target, level, internalformat, width, height, depth) {
    const i = Number(internalformat) >>> 0;
    defineTextureImage(texture, target, level, new TextureImage(i, i, 0, width, height, depth, true));
}

// `texStorage2D` / `texStorage3D` sent: every level of every face, each half the one before (a 2D array keeps its
// layers), and the texture immutable from now on.
function defineTextureStorage(texture, target, levels, internalformat, width, height, depth) {
    const t = Number(target) >>> 0;
    const i = Number(internalformat) >>> 0;
    const sized = _SIZED_UPLOADS.get(i);
    const first = t === 0x8513 ? 0x8515 : t;
    const last = t === 0x8513 ? 0x851a : t;
    for (let level = 0; level < levels; level++) {
        const image = new TextureImage(i, sized === undefined ? i : sized[0], sized === undefined ? 0 : sized[1][0],
            (width >> level) || 1, (height >> level) || 1, t === 0x806f ? (depth >> level) || 1 : depth,
            sized === undefined);
        for (let face = first; face <= last; face++) defineTextureImage(texture, face, level, image);
    }
    texture._immutableLevels = levels;
}

// Where WebGL samples a texture as incomplete -- (0, 0, 0, 1) -- that the OpenGL ES 3.0 driver underneath samples as
// complete, the facade withholds it from the draw. Two rules are WebGL's own:
//
// - WebGL 1 has no mipmaps or repeat of a size that is not a power of two (ES 2.0 3.8.2, "Texture Access"): a texture
//   whose level 0 is not a power of two each way is incomplete unless both its wraps are CLAMP_TO_EDGE and its
//   minification filter reads no mipmap.
// - A 32-bit float image is not filterable without OES_texture_float_linear, nor a WebGL 1 HALF_FLOAT_OES one without
//   OES_texture_half_float_linear (`_unfilterable`): a texture whose base image is one is incomplete while a filter of
//   it is not NEAREST -- the sampler's, where WebGL 2 has one bound to the unit, else its own. The driver filters
//   floats wherever it can.
//
// Which bindings are incomplete is kept per unit and target (`_incompleteBindings`, keys `unit * 4 + kind`, `unit` the
// unit's enum as the binding maps keep it, the kinds `_SAMPLED_TARGETS`'), after every call that can change one: an image of the base level defined, a filter, wrap or base
// level set on the texture or on a sampler, a texture or a sampler bound, a delete, the extension enabled.
const _SAMPLED_TARGETS = [0x0de1, 0x8513, 0x806f, 0x8c1a];          // TEXTURE_2D, TEXTURE_CUBE_MAP, TEXTURE_3D, 2D_ARRAY
const _FLOAT32_FORMATS = [0x822e, 0x8230, 0x8815, 0x8814];          // R32F, RG32F, RGB32F, RGBA32F

// What a filter or wrap of `params` (a texture's or a sampler's) is: what was set, or its initial value.
function _samplingParameter(params, pname) {
    const value = params === null || params === undefined ? undefined : params.get(pname);
    return value === undefined ? _SAMPLER_PARAMETERS.get(pname).initial : value;
}

// Whether `texture`, bound at `unit`, samples as incomplete by one of WebGL's own rules.
function samplesBlack(ctx, texture, unit) {
    const target = texture._target;
    const params = texture._params;
    if (!ctx._webgl2) {
        const image = ctx._image(texture, target === 0x8513 ? 0x8515 : target, 0);
        const minFilter = _samplingParameter(params, 0x2801);
        if (image !== undefined && !(_isPowerOfTwo(image.width) && _isPowerOfTwo(image.height)) &&
                !(_samplingParameter(params, 0x2802) === 0x812f && _samplingParameter(params, 0x2803) === 0x812f &&
                    (minFilter === 0x2600 || minFilter === 0x2601))) return true;
    }
    const base = ctx._webgl2 ? _samplingParameterOf(params, 0x813c, 0) : 0;   // TEXTURE_BASE_LEVEL
    const image = ctx._image(texture, target === 0x8513 ? 0x8515 : target, base);
    if (image === undefined || !_unfilterable(ctx, image)) return false;
    const sampler = ctx._webgl2 ? ctx._samplerBindings.get(unit - 0x84c0) : undefined;
    const filters = sampler === undefined ? params : sampler._parameters;
    const minFilter = _samplingParameter(filters, 0x2801);
    return _samplingParameter(filters, 0x2800) !== 0x2600 || (minFilter !== 0x2600 && minFilter !== 0x2700);
}

// Whether `image` is one `ctx` cannot filter: a 32-bit float one -- WebGL 2's sized ones, WebGL 1's FLOAT uploads --
// without OES_texture_float_linear, a WebGL 1 HALF_FLOAT_OES one without OES_texture_half_float_linear.
function _unfilterable(ctx, image) {
    if (image.compressed) return false;
    if (!ctx._webgl2 && image.type === 0x8d61) return ctx._oesTextureHalfFloatLinear === undefined;
    return ctx._oesTextureFloatLinear === undefined &&
        (_listHas(_FLOAT32_FORMATS, image.internalformat) || (!ctx._webgl2 && image.type === _FLOAT));
}

// A texture parameter that is not a sampler's: what was set, or `initial`.
function _samplingParameterOf(params, pname, initial) {
    const value = params === null || params === undefined ? undefined : params.get(pname);
    return value === undefined ? initial : value;
}

// The binding of `kind` at `unit` judged again.
function refreshBindingSampling(ctx, unit, kind) {
    const bindings = ctx._textureBindings(_SAMPLED_TARGETS[kind]);
    const texture = bindings === undefined ? undefined : bindings.get(unit);
    const key = unit * 4 + kind;
    if (texture !== undefined && texture !== null && samplesBlack(ctx, texture, unit)) ctx._incompleteBindings.add(key);
    else ctx._incompleteBindings.delete(key);
}

// Every binding of `texture` judged again: its base image, a filter, a wrap or its base level changed.
function refreshTextureSampling(ctx, texture) {
    if (texture._target === undefined) return;
    const kind = _SAMPLED_TARGETS.indexOf(texture._target);
    for (const [unit, bound] of ctx._textureBindings(texture._target)) {
        if (bound === texture) refreshBindingSampling(ctx, unit, kind);
    }
}

// Every binding of `unit` (an enum, TEXTURE0 + i) judged again: a sampler bound to it, or changed while bound.
function refreshUnitSampling(ctx, unit) {
    for (let kind = 0; kind < _SAMPLED_TARGETS.length; kind++) refreshBindingSampling(ctx, unit, kind);
}

// Every unit `sampler` is bound to judged again when `pname`, a filter of it, changed.
function refreshSamplerSampling(ctx, sampler, pname) {
    if (pname !== 0x2800 && pname !== 0x2801) return;
    for (const [index, bound] of ctx._samplerBindings) {
        if (bound === sampler) refreshUnitSampling(ctx, 0x84c0 + index);
    }
}

// Every binding judged again: OES_texture_float_linear or OES_texture_half_float_linear enabled.
function refreshAllSampling(ctx) {
    for (let kind = 0; kind < _SAMPLED_TARGETS.length; kind++) {
        const bindings = ctx._textureBindings(_SAMPLED_TARGETS[kind]);
        if (bindings === undefined) continue;
        for (const unit of bindings.keys()) refreshBindingSampling(ctx, unit, kind);
    }
}

// A draw samples nothing where an incomplete texture is bound: for the draw, each such binding holds texture 0, which
// WebGL never gives an image -- incomplete, and sampled as ES 3.0 has an incomplete texture sampled, (0, 0, 0, 1) --
// and is put back after it, with the active unit. Only commands cross; the facade's bindings stay.
// The caller checks `_incompleteBindings` is not empty, so a draw with none pays one size test.
function withholdIncompleteTextures(ctx) {
    for (const key of ctx._incompleteBindings) {
        encodeActiveTexture(ctx._canvasId, key >>> 2);
        encodeBindTexture(ctx._canvasId, _SAMPLED_TARGETS[key & 3], -1);
    }
}

function restoreIncompleteTextures(ctx) {
    for (const key of ctx._incompleteBindings) {
        const target = _SAMPLED_TARGETS[key & 3];
        const unit = key >>> 2;
        encodeActiveTexture(ctx._canvasId, unit);
        encodeBindTexture(ctx._canvasId, target, ctx._textureBindings(target).get(unit)._id);
    }
    encodeActiveTexture(ctx._canvasId, ctx._activeTextureUnit);
}

// ---- Vertex ranges -----------------------------------------------------------------------------------------------------
// A draw may read no vertex outside the buffers of the attributes the program in use consumes (WebGL 1.0 6.6, kept by
// WebGL 2.0): a vertex past one is INVALID_OPERATION and nothing is drawn. An indexed draw reads the vertices its indices
// name, so the facade keeps the bytes of every element-array buffer -- WebGL lets such a buffer take data only through
// `bufferData`, `bufferSubData` and a copy from another element-array buffer, all of which pass here -- and the largest
// index of each range a draw has read, until the buffer's data changes.

// The bytes of an element-array buffer change: its range cache goes.
function elementBytesChanged(buffer) {
    buffer._indexRanges = null;
}

// The largest index `count` indices of `bytes` per index (1, 2 or 4) from byte `offset` hold, in an element-array
// buffer; -1 for none. In WebGL 2 the primitive-restart index (all ones) restarts the primitive and reads no vertex.
function largestIndex(buffer, bytes, offset, count, restart) {
    let byOffset = buffer._indexRanges === null ? undefined : buffer._indexRanges.get(offset);
    const key = count * 8 + bytes * 2 + (restart ? 1 : 0);
    const cached = byOffset === undefined ? undefined : byOffset.get(key);
    if (cached !== undefined) return cached;
    const data = buffer._elements;
    const at = TypedArrayPrototypeGetByteOffset(data) + offset;
    const view = bytes === 1 ? new Uint8Array(TypedArrayPrototypeGetBuffer(data), at, count)
        : bytes === 2 ? new Uint16Array(TypedArrayPrototypeGetBuffer(data), at, count)
            : new Uint32Array(TypedArrayPrototypeGetBuffer(data), at, count);
    const skip = !restart ? -1 : bytes === 1 ? 0xff : bytes === 2 ? 0xffff : 0xffffffff;
    let largest = -1;
    for (let k = 0; k < count; k++) {
        const index = view[k];
        if (index > largest && index !== skip) largest = index;
    }
    if (buffer._indexRanges === null) buffer._indexRanges = new Map();
    if (byOffset === undefined) {
        byOffset = new Map();
        buffer._indexRanges.set(offset, byOffset);
    }
    byOffset.set(key, largest);
    return largest;
}

// Bytes of one component of a vertex attribute of `type` (`_attribPointerAccepted`'s types).
function _attribComponentBytes(type) {
    return type === 0x1400 || type === 0x1401 ? 1 : type === 0x1402 || type === 0x1403 || type === 0x140b ? 2 : 4;
}

// The compressed formats, each [block width, block height, bytes a block, the extension that enables it]. A format is
// one a call takes only while its extension is enabled (WebGL 1.0 5.14.8).
const _COMPRESSED_FORMATS = new Map();
// WEBGL_compressed_texture_etc1: COMPRESSED_RGB_ETC1_WEBGL, a 2D image defined whole -- by compressedTexImage2D or 2D
// immutable storage; a sub-image upload, a 3D call or 3D storage of it is INVALID_OPERATION, as a browser has it.
_COMPRESSED_FORMATS.set(0x8d64, [4, 4, 8, "etc1"]);
// WEBGL_compressed_texture_etc: R11, SIGNED_R11, RG11, SIGNED_RG11 (EAC); RGB8, SRGB8, the two PUNCHTHROUGH_ALPHA1,
// RGBA8 and SRGB8_ALPHA8 (ETC2).
[8, 8, 16, 16, 8, 8, 8, 8, 16, 16].forEach((bytes, k) => _COMPRESSED_FORMATS.set(0x9270 + k, [4, 4, bytes, "etc"]));
// WEBGL_compressed_texture_astc: the fourteen block sizes, linear (COMPRESSED_RGBA_ASTC_*) and sRGB.
[[4, 4], [5, 4], [5, 5], [6, 5], [6, 6], [8, 5], [8, 6], [8, 8], [10, 5], [10, 6], [10, 8], [10, 10], [12, 10], [12, 12]]
    .forEach(([w, h], k) => {
        _COMPRESSED_FORMATS.set(0x93b0 + k, [w, h, 16, "astc"]);
        _COMPRESSED_FORMATS.set(0x93d0 + k, [w, h, 16, "astc"]);
    });

// The bytes of a compressed image of `width` x `height` x `depth` texels: whole blocks over each layer.
function _compressedImageBytes(block, width, height, depth) {
    return MathCeil(width / block[0]) * MathCeil(height / block[1]) * block[2] * depth;
}

// The sized internal formats that are both colour-renderable and texture-filterable (ES 3.0 table 3.13), which
// `generateMipmap` takes besides the unsized ones and the float ones it can render and filter (`_mipmappable`).
const _MIPMAPPABLE_SIZED_FORMATS = [0x8229, 0x822b, 0x8051, 0x8d62, 0x8056, 0x8057, 0x8058, 0x8059, 0x8c43];

// The internal formats WebGL 2's `copyTexImage2D` takes: the unsized five and the colour-renderable sized ones are 0 --
// a float one only while it is colour-renderable (`floatColourRenderable`), INVALID_ENUM otherwise, as a browser has
// them --, a depth or stencil format INVALID_OPERATION, anything else INVALID_ENUM. The decoder
// (`copy_tex_image_format_error`) takes the float ones, as a context with the extension does.
function _copyTexImageFormatError(ctx, internalformat) {
    switch (internalformat) {
        case 0x1906: case 0x1907: case 0x1908: case 0x1909: case 0x190a:                       // the unsized five
        case 0x8229: case 0x822b: case 0x8051: case 0x8056: case 0x8057: case 0x8058: case 0x8059: case 0x8d62:
        case 0x8c41: case 0x8c43:                                                               // R8 .. SRGB8_ALPHA8
        case 0x8231: case 0x8232: case 0x8233: case 0x8234: case 0x8235: case 0x8236:
        case 0x8237: case 0x8238: case 0x8239: case 0x823a: case 0x823b: case 0x823c:           // R*/RG* integer
        case 0x8d70: case 0x8d76: case 0x8d7c: case 0x8d82: case 0x8d88: case 0x8d8e: case 0x906f:   // RGBA* integer
            return 0;
        case 0x822d: case 0x822f: case 0x822e: case 0x8230: case 0x8814: case 0x881a: case 0x8c3a:   // the float ones
            return floatColourRenderable(ctx, _FORMAT_INFO.get(internalformat)) ? 0 : GL_INVALID_ENUM;
        case 0x1902: case 0x81a5: case 0x81a6: case 0x8cac: case 0x84f9: case 0x88f0: case 0x8cad:   // depth, stencil
            return GL_INVALID_OPERATION;
        default:
            return GL_INVALID_ENUM;
    }
}

// ---- Framebuffer completeness --------------------------------------------------------------------------------------
//
// What a framebuffer object's attachments make it, judged here from what the facade recorded -- the attachments
// (`_noteAttachment`), each texture's images (`TextureImage`) and each renderbuffer's storage -- by ES 3.0 4.4.4 and
// WebGL's own rules (WebGL 1.0 6.6): every call that draws into or reads from a framebuffer that is not
// complete is INVALID_FRAMEBUFFER_OPERATION before anything is sent. The renderer's driver would refuse the call too,
// but its error never reaches `getError`, and the facade would have recorded what the call defines as if it had run.

// Each sized internal format: its channel sizes in bits -- red, green, blue, alpha, depth, stencil --, its component
// type (`_N` unsigned normalized, `_S` signed normalized, `_F` float, `_I` signed integer, `_U` unsigned integer), and
// flags: colour-renderable (ES 3.0 table 3.13), colour-renderable once EXT_color_buffer_float is enabled, and sRGB.
const _N = 0, _S = 1, _F = 2, _I = 3, _U = 4;
// `_FLOAT_RENDERABLE`: colour-renderable once EXT_color_buffer_float is enabled; `_HALF_RENDERABLE` too, a 16-bit float
// one, once EXT_color_buffer_half_float is (`floatColourRenderable`).
const _RENDERABLE = 1, _SRGB = 2, _FLOAT_RENDERABLE = 4, _HALF_RENDERABLE = 8;
const _FORMAT_INFO = new Map([
    [0x8229, [8, 0, 0, 0, 0, 0, _N, _RENDERABLE]],              // R8
    [0x822b, [8, 8, 0, 0, 0, 0, _N, _RENDERABLE]],              // RG8
    [0x8051, [8, 8, 8, 0, 0, 0, _N, _RENDERABLE]],              // RGB8
    [0x8d62, [5, 6, 5, 0, 0, 0, _N, _RENDERABLE]],              // RGB565
    [0x8056, [4, 4, 4, 4, 0, 0, _N, _RENDERABLE]],              // RGBA4
    [0x8057, [5, 5, 5, 1, 0, 0, _N, _RENDERABLE]],              // RGB5_A1
    [0x8058, [8, 8, 8, 8, 0, 0, _N, _RENDERABLE]],              // RGBA8
    [0x8059, [10, 10, 10, 2, 0, 0, _N, _RENDERABLE]],           // RGB10_A2
    [0x906f, [10, 10, 10, 2, 0, 0, _U, _RENDERABLE]],           // RGB10_A2UI
    [0x8c43, [8, 8, 8, 8, 0, 0, _N, _RENDERABLE | _SRGB]],      // SRGB8_ALPHA8
    [0x8c41, [8, 8, 8, 0, 0, 0, _N, _SRGB]],                    // SRGB8
    [0x8f94, [8, 0, 0, 0, 0, 0, _S, 0]],                        // R8_SNORM
    [0x8f95, [8, 8, 0, 0, 0, 0, _S, 0]],                        // RG8_SNORM
    [0x8f96, [8, 8, 8, 0, 0, 0, _S, 0]],                        // RGB8_SNORM
    [0x8f97, [8, 8, 8, 8, 0, 0, _S, 0]],                        // RGBA8_SNORM
    [0x8232, [8, 0, 0, 0, 0, 0, _U, _RENDERABLE]],              // R8UI
    [0x8231, [8, 0, 0, 0, 0, 0, _I, _RENDERABLE]],              // R8I
    [0x8234, [16, 0, 0, 0, 0, 0, _U, _RENDERABLE]],             // R16UI
    [0x8233, [16, 0, 0, 0, 0, 0, _I, _RENDERABLE]],             // R16I
    [0x8236, [32, 0, 0, 0, 0, 0, _U, _RENDERABLE]],             // R32UI
    [0x8235, [32, 0, 0, 0, 0, 0, _I, _RENDERABLE]],             // R32I
    [0x8238, [8, 8, 0, 0, 0, 0, _U, _RENDERABLE]],              // RG8UI
    [0x8237, [8, 8, 0, 0, 0, 0, _I, _RENDERABLE]],              // RG8I
    [0x823a, [16, 16, 0, 0, 0, 0, _U, _RENDERABLE]],            // RG16UI
    [0x8239, [16, 16, 0, 0, 0, 0, _I, _RENDERABLE]],            // RG16I
    [0x823c, [32, 32, 0, 0, 0, 0, _U, _RENDERABLE]],            // RG32UI
    [0x823b, [32, 32, 0, 0, 0, 0, _I, _RENDERABLE]],            // RG32I
    [0x8d7c, [8, 8, 8, 8, 0, 0, _U, _RENDERABLE]],              // RGBA8UI
    [0x8d8e, [8, 8, 8, 8, 0, 0, _I, _RENDERABLE]],              // RGBA8I
    [0x8d76, [16, 16, 16, 16, 0, 0, _U, _RENDERABLE]],          // RGBA16UI
    [0x8d88, [16, 16, 16, 16, 0, 0, _I, _RENDERABLE]],          // RGBA16I
    [0x8d70, [32, 32, 32, 32, 0, 0, _U, _RENDERABLE]],          // RGBA32UI
    [0x8d82, [32, 32, 32, 32, 0, 0, _I, _RENDERABLE]],          // RGBA32I
    [0x8d7d, [8, 8, 8, 0, 0, 0, _U, 0]],                        // RGB8UI
    [0x8d8f, [8, 8, 8, 0, 0, 0, _I, 0]],                        // RGB8I
    [0x8d77, [16, 16, 16, 0, 0, 0, _U, 0]],                     // RGB16UI
    [0x8d89, [16, 16, 16, 0, 0, 0, _I, 0]],                     // RGB16I
    [0x8d71, [32, 32, 32, 0, 0, 0, _U, 0]],                     // RGB32UI
    [0x8d83, [32, 32, 32, 0, 0, 0, _I, 0]],                     // RGB32I
    [0x822d, [16, 0, 0, 0, 0, 0, _F, _FLOAT_RENDERABLE | _HALF_RENDERABLE]],       // R16F
    [0x822f, [16, 16, 0, 0, 0, 0, _F, _FLOAT_RENDERABLE | _HALF_RENDERABLE]],      // RG16F
    [0x881b, [16, 16, 16, 0, 0, 0, _F, 0]],                     // RGB16F
    [0x881a, [16, 16, 16, 16, 0, 0, _F, _FLOAT_RENDERABLE | _HALF_RENDERABLE]],    // RGBA16F
    [0x822e, [32, 0, 0, 0, 0, 0, _F, _FLOAT_RENDERABLE]],       // R32F
    [0x8230, [32, 32, 0, 0, 0, 0, _F, _FLOAT_RENDERABLE]],      // RG32F
    [0x8815, [32, 32, 32, 0, 0, 0, _F, 0]],                     // RGB32F
    [0x8814, [32, 32, 32, 32, 0, 0, _F, _FLOAT_RENDERABLE]],    // RGBA32F
    [0x8c3a, [11, 11, 10, 0, 0, 0, _F, _FLOAT_RENDERABLE]],     // R11F_G11F_B10F
    [0x8c3d, [9, 9, 9, 0, 0, 0, _F, 0]],                        // RGB9_E5
    [0x81a5, [0, 0, 0, 0, 16, 0, _N, 0]],                       // DEPTH_COMPONENT16
    [0x81a6, [0, 0, 0, 0, 24, 0, _N, 0]],                       // DEPTH_COMPONENT24
    [0x8cac, [0, 0, 0, 0, 32, 0, _F, 0]],                       // DEPTH_COMPONENT32F
    [0x88f0, [0, 0, 0, 0, 24, 8, _N, 0]],                       // DEPTH24_STENCIL8
    [0x8cad, [0, 0, 0, 0, 32, 8, _F, 0]],                       // DEPTH32F_STENCIL8
    [0x8d48, [0, 0, 0, 0, 0, 8, _U, 0]],                        // STENCIL_INDEX8
    [0x84f9, [0, 0, 0, 0, 16, 8, _N, 0]],                       // DEPTH_STENCIL (WebGL 1's renderbuffer format)
]);

// The sized format an image of an unsized internal format has (ES 3.0 table 3.12, WebGL 1's unsized uploads and its
// extensions' -- RGBA and RGB floats and half-floats, sRGB, depth), from the format and type its data was given in;
// undefined for one no framebuffer can have as a colour, depth or stencil buffer (LUMINANCE, ALPHA and their mix, of
// any type, a compressed image).
function _effectiveFormat(image) {
    if (image.compressed) return undefined;
    const i = image.internalformat;
    if (_FORMAT_INFO.has(i)) return i;
    switch (i) {
        case 0x1908:            // RGBA
            return image.type === _UBYTE ? 0x8058 : image.type === 0x8033 ? 0x8056 : image.type === 0x8034 ? 0x8057
                : image.type === _FLOAT ? 0x8814 : image.type === 0x8d61 ? 0x881a : undefined;
        case 0x1907:            // RGB
            return image.type === _UBYTE ? 0x8051 : image.type === 0x8363 ? 0x8d62
                : image.type === _FLOAT ? 0x8815 : image.type === 0x8d61 ? 0x881b : undefined;
        case 0x8c42:            // SRGB_ALPHA_EXT
            return 0x8c43;
        case 0x8c40:            // SRGB_EXT
            return 0x8c41;
        case 0x1902:            // DEPTH_COMPONENT
            return image.type === _USHORT ? 0x81a5 : image.type === _UINT ? 0x81a6 : undefined;
        case 0x84f9:            // DEPTH_STENCIL
            return image.type === 0x84fa ? 0x88f0 : undefined;     // UNSIGNED_INT_24_8
        default:
            return undefined;
    }
}

// Bumped by everything that can change what a framebuffer's attachments are: an attachment, a renderbuffer's
// storage, a texture's image. A framebuffer keeps the status it was last judged to have with the generation it was
// judged at, so a judgement is made once a change, not once a call.
let _framebufferGeneration = 0;
function framebufferChanged() {
    _framebufferGeneration++;
}

const GL_FRAMEBUFFER_COMPLETE = 0x8cd5;
const GL_INVALID_FRAMEBUFFER_OPERATION = 0x0506;

// What one attachment record is: [sized format, width, height, samples], or undefined when it is not an image a
// framebuffer can use -- a texture level with no image or a zero-sized one, a layer past the image's depth, a
// renderbuffer with no storage --, null when it is one no framebuffer can use: of a format nothing renders, or a level
// its texture does not let be attached (`_attachableLevel`).
function _attachmentImage(ctx, record) {
    const object = record.object;
    if (record.type === 0x8d41) {                       // RENDERBUFFER
        if (object._format === undefined || !object._width || !object._height) return undefined;
        return [object._format, object._width, object._height, object._samples | 0];
    }
    if (object._target === undefined) return undefined;
    const target = object._target === 0x8513 ? record.face : object._target;
    const image = ctx._image(object, target, record.level);
    if (image === undefined || image.width === 0 || image.height === 0) return undefined;
    if ((object._target === 0x806f || object._target === 0x8c1a) && (record.layer | 0) >= image.depth) return undefined;
    const format = _effectiveFormat(image);
    return format === undefined || !_attachableLevel(ctx, object, record.level) ? null
        : [format, image.width, image.height, 0];
}

// Whether `level` of `texture` may be attached (ES 3.0 4.4.4.2): any of immutable storage's; otherwise one from the base
// level to the last a full chain from it has (or TEXTURE_MAX_LEVEL), and one other than the base only of a texture
// that is mipmap complete. A cube map's faces must be cube complete.
function _attachableLevel(ctx, texture, level) {
    if (texture._immutableLevels !== 0) return true;
    const base = ctx._baseLevel(texture);
    const target = texture._target;
    const first = target === 0x8513 ? 0x8515 : target;
    const top = ctx._image(texture, first, base);
    if (top === undefined || level < base) return false;
    const threeD = target === 0x806f;
    const last = MathMin(ctx._maxLevel(texture),
        base + maxMipLevels(MathMax(top.width, top.height, threeD ? top.depth : 1)) - 1);
    if (level > last) return false;
    if (level === base) return target !== 0x8513 || _levelsComplete(ctx, texture, top, base, base);
    return _levelsComplete(ctx, texture, top, base, last);
}

// Whether every face of `texture` has, from `base` to `last`, the image a chain from `top` (the first face's base
// image) has: each half the one before, of `top`'s format.
function _levelsComplete(ctx, texture, top, base, last) {
    const target = texture._target;
    const first = target === 0x8513 ? 0x8515 : target;
    const lastFace = target === 0x8513 ? 0x851a : target;
    const threeD = target === 0x806f;
    for (let face = first; face <= lastFace; face++) {
        for (let level = base; level <= last; level++) {
            const image = ctx._image(texture, face, level);
            const shift = level - base;
            if (image === undefined || image.compressed !== top.compressed ||
                    image.internalformat !== top.internalformat ||
                    (!_FORMAT_INFO.has(top.internalformat) && image.type !== top.type) ||
                    image.width !== ((top.width >> shift) || 1) || image.height !== ((top.height >> shift) || 1) ||
                    image.depth !== (threeD ? (top.depth >> shift) || 1 : top.depth)) return false;
        }
    }
    return true;
}

// FRAMEBUFFER_COMPLETE, or the first way `fb` is not, in the order a browser finds them: an attachment that is not an
// image of a format its point can render to (INCOMPLETE_ATTACHMENT), no attachment at all (MISSING_ATTACHMENT), in
// WebGL 1 attachments of different sizes (DIMENSIONS), depth and stencil attachments that are not one image
// (UNSUPPORTED), renderbuffers of different sample counts (MULTISAMPLE). The default framebuffer is complete.
function framebufferStatus(ctx, fb) {
    if (fb === null) return GL_FRAMEBUFFER_COMPLETE;
    if (fb._statusGeneration === _framebufferGeneration) return fb._status;
    let status = GL_FRAMEBUFFER_COMPLETE;
    let attached = 0, width = -1, height = -1, samples = -1, dimensions = false, multisample = false;
    if (fb._attachments) {
        for (const [point, record] of fb._attachments) {
            attached++;
            const image = _attachmentImage(ctx, record);
            const info = image ? _FORMAT_INFO.get(image[0]) : undefined;
            let renders = false;
            if (info === undefined) {
                renders = false;
            } else if (point === 0x8d00 || point === 0x8d20) {
                renders = point === 0x8d00 ? info[4] > 0 : info[5] > 0;
                // WebGL 1.0 6.6: a DEPTH_STENCIL image goes to DEPTH_STENCIL_ATTACHMENT, a depth-only or stencil-only
                // one to its own point, and no other way round.
                if (renders && !ctx._webgl2) renders = (record.point === 0x821a) === (info[4] > 0 && info[5] > 0);
            } else {
                renders = colorRenderable(ctx, info);
            }
            if (!renders) {
                status = 0x8cd6;                        // INCOMPLETE_ATTACHMENT
                break;
            }
            if (width < 0) {
                width = image[1];
                height = image[2];
                samples = image[3];
            } else {
                if (image[1] !== width || image[2] !== height) dimensions = true;
                if (image[3] !== samples) multisample = true;
            }
        }
    }
    if (status === GL_FRAMEBUFFER_COMPLETE) {
        const depth = fb._attachments && fb._attachments.get(0x8d00);
        const stencil = fb._attachments && fb._attachments.get(0x8d20);
        if (attached === 0) status = 0x8cd7;                                    // MISSING_ATTACHMENT
        else if (dimensions && !ctx._webgl2) status = 0x8cd9;                   // INCOMPLETE_DIMENSIONS
        else if (depth && stencil && (depth.object !== stencil.object || depth.level !== stencil.level ||
            depth.face !== stencil.face || (depth.layer | 0) !== (stencil.layer | 0))) status = 0x8cdd;   // UNSUPPORTED
        else if (multisample) status = 0x8d56;                                 // INCOMPLETE_MULTISAMPLE
    }
    fb._status = status;
    fb._statusGeneration = _framebufferGeneration;
    return status;
}

// Whether a format (`_FORMAT_INFO`'s entry) is colour-renderable in `ctx`: the float ones only with an extension that
// makes them so enabled (`floatColourRenderable`).
function colorRenderable(ctx, info) {
    return (info[7] & _RENDERABLE) !== 0 || floatColourRenderable(ctx, info);
}

// Whether a float format (`_FORMAT_INFO`'s entry) is colour-renderable in `ctx`: every one with EXT_color_buffer_float
// enabled, the 16-bit ones with EXT_color_buffer_half_float, and in WebGL 1 -- whose only 32-bit one that renders is
// RGBA32F -- the 32-bit ones with WEBGL_color_buffer_float.
function floatColourRenderable(ctx, info) {
    const flags = info[7];
    return (flags & _FLOAT_RENDERABLE) !== 0 && (ctx._extColorBufferFloat !== undefined ||
        ((flags & _HALF_RENDERABLE) !== 0 ? ctx._extColorBufferHalfFloat : ctx._webglColorBufferFloat) !== undefined);
}

// INVALID_FRAMEBUFFER_OPERATION recorded and true when `fb` is not complete.
function refusesIncompleteFramebuffer(ctx, fb) {
    if (framebufferStatus(ctx, fb) === GL_FRAMEBUFFER_COMPLETE) return false;
    recordGpuPreflightError(ctx._canvasId, GL_INVALID_FRAMEBUFFER_OPERATION);
    return true;
}

// The sized format of the colour buffer reads come from: the read framebuffer's read buffer -- the drawing buffer's
// own format for the default framebuffer -- or 0 when the read buffer is NONE or names no image.
function readColorFormat(ctx) {
    const fb = ctx._readFramebufferBinding;
    if (fb === null) return ctx._defaultReadBuffer === 0 ? 0 : ctx._drawingBufferFormat;
    const point = fb._readBuffer === undefined ? 0x8ce0 : fb._readBuffer;
    if (point === 0) return 0;
    const record = fb._attachments ? fb._attachments.get(point) : undefined;
    const image = record ? _attachmentImage(ctx, record) : undefined;
    return image ? image[0] : 0;
}

// Whether a copy from a read buffer of the sized format `source` may make or fill an image of `internalformat` (ES 3.0
// 3.8.5, table 3.15, as a browser enforces it): an unsized format takes a linear, unsigned-normalized source that has
// the components it needs -- luminance is red --; a sized one takes a source with each component it has, of the same
// component type, colour encoding and size.
function copyCompatible(source, internalformat) {
    const src = _FORMAT_INFO.get(source);
    if (src === undefined) return false;
    const linearNormalized = src[6] === _N && (src[7] & _SRGB) === 0;
    switch (internalformat) {
        case 0x1906: return linearNormalized && src[3] > 0;                     // ALPHA
        case 0x1909: return linearNormalized && src[0] > 0;                     // LUMINANCE
        case 0x190a: return linearNormalized && src[0] > 0 && src[3] > 0;       // LUMINANCE_ALPHA
        case 0x1907: return linearNormalized && src[2] > 0;                     // RGB
        case 0x1908: return linearNormalized && src[2] > 0 && src[3] > 0;       // RGBA
        default: break;
    }
    const dst = _FORMAT_INFO.get(internalformat);
    if (dst === undefined || dst[4] > 0 || dst[5] > 0 || dst[6] !== src[6] || (dst[7] & _SRGB) !== (src[7] & _SRGB)) {
        return false;
    }
    for (let c = 0; c < 4; c++) if (dst[c] !== 0 && dst[c] !== src[c]) return false;
    return true;
}

// The bytes a compressed upload's source (`_compressedUploadSource`: bytes, buffer offset, size) carries.
function _compressedSourceBytes(source) {
    return source[1] >= 0 ? source[2] : TypedArrayPrototypeGetByteLength(source[0]);
}

// The internal formats a renderbuffer takes: WebGL 1's six (WebGL 1.0 5.14.7) and its extensions' (`renderbufferStorage`),
// and in WebGL 2 every colour-renderable
// sized format of ES 3.0 table 3.13 that needs no extension, the depth and stencil ones of table 3.14, and WebGL 1's
// DEPTH_STENCIL.
const _WEBGL1_RENDERBUFFER_FORMATS = [0x8056, 0x8d62, 0x8057, 0x81a5, 0x8d48, 0x84f9];
const _WEBGL2_RENDERBUFFER_FORMATS = [
    0x8229, 0x822b, 0x8051, 0x8d62, 0x8056, 0x8057, 0x8058, 0x8059, 0x906f, 0x8c43,     // R8 .. SRGB8_ALPHA8
    0x8231, 0x8232, 0x8233, 0x8234, 0x8235, 0x8236, 0x8237, 0x8238, 0x8239, 0x823a, 0x823b, 0x823c,   // R*/RG* integer
    0x8d8e, 0x8d7c, 0x8d88, 0x8d76, 0x8d82, 0x8d70,                                     // RGBA* integer
    0x81a5, 0x81a6, 0x8cac, 0x88f0, 0x8cad, 0x8d48, 0x84f9,                             // depth and stencil
];

// A `texImage2D`'s level, size and border, around the error its formats are (`_uploadFormatError`), in the order a
// browser judges them: a level out of range is INVALID_VALUE, then the formats' error, then a size or border out of
// range INVALID_VALUE. True when none is; the error recorded when one is. The target is the caller's to have checked.
function preflightTexImage2D(canvasId, target, level, width, height, border, formatError) {
    if (!NumberIsInteger(level) || level < 0 || level >= MAX_WEBGL_GPU_2D_LEVELS) {
        return recordGpuPreflightError(canvasId, GL_INVALID_VALUE);
    }
    if (formatError !== 0) return recordGpuPreflightError(canvasId, formatError);
    const isCubeFace = target >= 0x8515 && target <= 0x851A;
    const maxAtLevel = (MAX_WEBGL_GPU_2D_DIMENSION >>> level) || 1;
    if (!NumberIsInteger(width) || width < 0 || width > maxAtLevel ||
        !NumberIsInteger(height) || height < 0 || height > maxAtLevel ||
        (isCubeFace && width !== height) || border !== 0) {
        return recordGpuPreflightError(canvasId, GL_INVALID_VALUE);
    }
    return true;
}

// A sub-image upload's level, offsets and size, around the error its formats are (`_subUploadFormatError`), in the
// order `preflightTexImage2D` judges them: a level past the last one is INVALID_VALUE, then the formats' error, then
// anything negative INVALID_VALUE. That the region lies inside the level's image is the driver's to judge. The target is
// the caller's to have checked.
function preflightTexSubImage(canvasId, level, levels, formatError, xoffset, yoffset, zoffset, width, height, depth) {
    if (!NumberIsInteger(level) || level < 0 || level >= levels) {
        return recordGpuPreflightError(canvasId, GL_INVALID_VALUE);
    }
    if (formatError !== 0) return recordGpuPreflightError(canvasId, formatError);
    if (!NumberIsInteger(xoffset) || xoffset < 0 || !NumberIsInteger(yoffset) || yoffset < 0 ||
        !NumberIsInteger(zoffset) || zoffset < 0 ||
        !NumberIsInteger(width) || width < 0 || !NumberIsInteger(height) || height < 0 ||
        !NumberIsInteger(depth) || depth < 0) {
        return recordGpuPreflightError(canvasId, GL_INVALID_VALUE);
    }
    return true;
}

// `texStorage2D`'s levels and size, after the error its internal format is (`_storageFormatError`): no level, a size
// not above 0 or past the limit, more levels than the size has, or a cube map that is not square is INVALID_VALUE. The
// target is the caller's to have checked (`_textureFor`, "storage2D").
function preflightTexStorage2D(canvasId, target, levels, formatError, width, height) {
    if (formatError !== 0) return recordGpuPreflightError(canvasId, formatError);
    if (!NumberIsInteger(levels) || levels <= 0 ||
        !NumberIsInteger(width) || width <= 0 || width > MAX_WEBGL_GPU_2D_DIMENSION ||
        !NumberIsInteger(height) || height <= 0 || height > MAX_WEBGL_GPU_2D_DIMENSION ||
        (target === 0x8513 && width !== height) ||
        levels > maxMipLevels(width > height ? width : height)) {
        return recordGpuPreflightError(canvasId, GL_INVALID_VALUE);
    }
    return true;
}

// As `preflightTexStorage2D`, for a 3D or 2D-array texture (`_textureFor`, "image3D").
function preflightTexStorage3D(canvasId, target, levels, formatError, width, height, depth) {
    if (formatError !== 0) return recordGpuPreflightError(canvasId, formatError);
    const maxXY = target === 0x806F ? MAX_WEBGL_GPU_3D_DIMENSION : MAX_WEBGL_GPU_2D_DIMENSION;
    const mipBasis = target === 0x806F
        ? (width > height ? (width > depth ? width : depth) : (height > depth ? height : depth))
        : (width > height ? width : height);
    if (!NumberIsInteger(levels) || levels <= 0 ||
        !NumberIsInteger(width) || width <= 0 || width > maxXY ||
        !NumberIsInteger(height) || height <= 0 || height > maxXY ||
        !NumberIsInteger(depth) || depth <= 0 ||
        depth > (target === 0x806F ? MAX_WEBGL_GPU_3D_DIMENSION : MAX_WEBGL_GPU_ARRAY_LAYERS) ||
        levels > maxMipLevels(mipBasis)) {
        return recordGpuPreflightError(canvasId, GL_INVALID_VALUE);
    }
    return true;
}

function allowWebglUpload(canvasId, byteLength) {
    if (!NumberIsFinite(byteLength) || byteLength < 0 || byteLength > MAX_WEBGL_UPLOAD_BYTES) {
        op_webgl_record_out_of_memory(canvasId);
        return false;
    }
    return true;
}

// An upload's bytes, or null with OUT_OF_MEMORY recorded for one past the bound. Both lanes refuse such an upload on
// their own, and the check here is not that one repeated: the calls record what an upload changes -- a buffer's size,
// an element buffer's bytes, a texture's image -- before they send it, and that record must not describe an upload
// the renderer never received. A plain sequence is judged by its declared length, before the conversion that would
// allocate it; a view is borrowed, not copied, unless its memory is shared.
function toBoundedUploadBytes(canvasId, input) {
    if (ArrayIsArray(input) && !allowWebglUpload(canvasId, input.length)) {
        return null;
    }
    let view = toUnit8Array(input);
    const byteLength = TypedArrayPrototypeGetByteLength(view);
    if (!allowWebglUpload(canvasId, byteLength)) return null;
    // Rust's bounded ops borrow before making their owned command copy.
    view = ensureNonSharedTypedArray(view, Uint8Array);
    return view;
}

function allowWebglShaderSource(canvasId, source) {
    if (typeof source === "string" && source.length > MAX_WEBGL_SHADER_SOURCE_CODE_UNITS) {
        op_webgl_record_out_of_memory(canvasId);
        return false;
    }
    return true;
}

function toTypedArray(input, Type) {
    if (isTypedArray(input)) {
        return new Type(
            TypedArrayPrototypeGetBuffer(input),
            TypedArrayPrototypeGetByteOffset(input),
            TypedArrayPrototypeGetByteLength(input) / Type.BYTES_PER_ELEMENT,
        );
    } else if (isDataView(input)) {
        return new Type(
            DataViewPrototypeGetBuffer(input),
            DataViewPrototypeGetByteOffset(input),
            DataViewPrototypeGetByteLength(input) / Type.BYTES_PER_ELEMENT,
        );
    } else if (isArrayBuffer(input)) {
        return new Type(
            input,
            0,
            ArrayBufferPrototypeGetByteLength(input) / Type.BYTES_PER_ELEMENT,
        );
    } else if (ArrayIsArray(input)) {
        // WebGL typed-list setters (uniform1iv/uniform4fv/... take an
        // `Int32List`/`Float32List`) accept a plain `sequence<GLint/GLfloat>`,
        // not only a TypedArray. Copy the array into the target typed array.
        // e.g. Phaser's multi-texture shader sets `uniform1iv(loc, [0,1,2,...])`.
        return new Type(input);
    }
    throw new TypeError("Invalid input: must be a TypedArray, DataView, or ArrayBuffer");
}

function toUnit8Array(input) {
    return toTypedArray(input, Uint8Array);
}

// Rust's fast `#[buffer]` path borrows the typed-array backing as a slice.
// A SharedArrayBuffer can be mutated concurrently by another isolate, which
// is not a valid Rust shared-slice contract. Copy shared views in V8 first;
// normal ArrayBuffer-backed target arrays retain the zero-copy path.
function ensureNonSharedTypedArray(view, Type) {
    return isSharedArrayBuffer(TypedArrayPrototypeGetBuffer(view))
        ? new Type(view)
        : view;
}

function toFloat32AsUint32(input) {
    let f32;
    if (isTypedArray(input)) {
        f32 = TypedArrayPrototypeGetSymbolToStringTag(input) === "Float32Array"
            ? input
            : new Float32Array(input);
    } else if (ArrayIsArray(input)) {
        // Float32List accepts numeric sequences. Constructing Uint32Array
        // directly would truncate each float to an integer before Rust
        // reinterprets the words, corrupting values such as 1.5.
        f32 = new Float32Array(input);
    } else if (isDataView(input)) {
        f32 = new Float32Array(
            DataViewPrototypeGetBuffer(input),
            DataViewPrototypeGetByteOffset(input),
            DataViewPrototypeGetByteLength(input) / Float32Array.BYTES_PER_ELEMENT,
        );
    } else if (isArrayBuffer(input)) {
        f32 = new Float32Array(
            input,
            0,
            ArrayBufferPrototypeGetByteLength(input) / Float32Array.BYTES_PER_ELEMENT,
        );
    } else {
        throw new TypeError("Invalid float list: must be a numeric sequence or buffer view");
    }
    f32 = ensureNonSharedTypedArray(f32, Float32Array);
    return new Uint32Array(
        TypedArrayPrototypeGetBuffer(f32),
        TypedArrayPrototypeGetByteOffset(f32),
        TypedArrayPrototypeGetByteLength(f32) / Uint32Array.BYTES_PER_ELEMENT,
    );
}

// Convert to an Int32 typed list, then expose the same bits to the fast
// borrowed u32 op. Rust copies those words into inline SmallVec storage.
function toInt32AsUint32(input) {
    let i32;
    if (isTypedArray(input)) {
        i32 = TypedArrayPrototypeGetSymbolToStringTag(input) === "Int32Array"
            ? input
            : new Int32Array(input);
    } else if (ArrayIsArray(input)) {
        i32 = new Int32Array(input);
    } else if (isDataView(input)) {
        i32 = new Int32Array(
            DataViewPrototypeGetBuffer(input),
            DataViewPrototypeGetByteOffset(input),
            DataViewPrototypeGetByteLength(input) / Int32Array.BYTES_PER_ELEMENT,
        );
    } else if (isArrayBuffer(input)) {
        i32 = new Int32Array(
            input,
            0,
            ArrayBufferPrototypeGetByteLength(input) / Int32Array.BYTES_PER_ELEMENT,
        );
    } else {
        throw new TypeError("Invalid integer list: must be a numeric sequence or buffer view");
    }
    i32 = ensureNonSharedTypedArray(i32, Int32Array);
    return new Uint32Array(
        TypedArrayPrototypeGetBuffer(i32),
        TypedArrayPrototypeGetByteOffset(i32),
        TypedArrayPrototypeGetByteLength(i32) / Uint32Array.BYTES_PER_ELEMENT,
    );
}

// The payload of a WebGL 2 uniform list call, `Type` being the element type the uniform takes (Uint32Array for the unsigned
// vectors, Float32Array for the matrices). WebGL 2 adds `srcOffset` and `srcLength` (in elements; a length of 0 means to the
// end) to every list setter, and the list has to be a whole number of `unit` elements and at least one: otherwise the call is
// INVALID_VALUE and changes nothing. Returns the words as the Uint32Array the stream copies, or `null` after recording the
// error. A value that is not a list is a TypeError, as WebIDL has it.
function _uniformListPayload(canvasId, name, data, srcOffset, srcLength, unit, Type) {
    let view;
    if (isTypedArray(data)) {
        view = data instanceof Type ? data : new Type(data);
    } else if (ArrayIsArray(data)) {
        view = new Type(data);
    } else {
        throw new TypeError(`Failed to execute '${name}' on 'WebGL2RenderingContext': parameter 3 is not of type '${Type.name.slice(0, -5)}List'.`);
    }
    const length = view.length;
    const offset = toUnsignedLongLong(srcOffset);
    let count = srcLength >>> 0;
    if (offset > length || (count !== 0 && offset + count > length)) {
        return recordGpuPreflightError(canvasId, GL_INVALID_VALUE) || null;
    }
    if (count === 0) count = length - offset;
    if (count < unit || count % unit !== 0) {
        return recordGpuPreflightError(canvasId, GL_INVALID_VALUE) || null;
    }
    view = view.subarray(offset, offset + count);
    view = ensureNonSharedTypedArray(view, Type);
    return new Uint32Array(
        TypedArrayPrototypeGetBuffer(view),
        TypedArrayPrototypeGetByteOffset(view),
        TypedArrayPrototypeGetByteLength(view) / Uint32Array.BYTES_PER_ELEMENT,
    );
}

// A canvas this runtime made: a numeric `_rid` (allocated by op_create_canvas) and a `getContext` method. Uploaded from
// its pixels on the GPU (`TEX_SOURCE_CANVAS`), never through `getImageData` and a readback.
function _migoIsHTMLCanvas(source) {
    return source
        && typeof source === "object"
        && typeof source._rid === "number"
        && typeof source.getContext === "function";
}

// The id of the 2D canvas whose pixels an upload from `source` reads: the canvas itself for a 2D one, and for a WebGL
// canvas (p5's filter copies its own WebGL canvas into a framebuffer's texture, three.js reads a second renderer's)
// the 2D canvas the Canvas keeps holding what it shows now. The renderer copies 2D canvases only.
function _migoSourceRid(source) {
    return typeof source._imageSourceCanvas === "function" ? source._imageSourceCanvas()._rid : source._rid;
}

// Text texture cache HIT: `getImageData` returned a synthetic
// ImageData carrying `__migo_text_cache_key__` (the offscreen
// fillText was suppressed).  Route straight to the cached-texture
// copy; the render thread unpins the entry after the GPU copy.
// Returns true when it handled the upload.
function _migoTexImageFromTextCache(canvasId, target, level, internalformat, format, type, src) {
    if (!src || typeof src !== "object") return false;
    const k = src.__migo_text_cache_key__;
    if (!k) return false;
    _rawTexImage2DFromTextCache(
        canvasId,
        target,
        level,
        internalformat,
        format,
        type,
        k.text, k.fontRequest, k.fontSize, k.fontWeight,
        k.italic, k.fillColor, k.textAlign, k.textBaseline,
        k.canvasW, k.canvasH,
    );
    // Single-shot: clear the marker so a re-upload of the same
    // ImageData object doesn't double-consume the (already unpinned)
    // entry.
    src.__migo_text_cache_key__ = null;
    return true;
}

// An `ImageData`'s RGBA8 rows, or those of a canvas this runtime did not make, through its 2D context; null for a value
// that has neither.
function sourceToRawRgba(source) {
    if (!source || typeof source !== "object") return null;

    const width = source.width | 0;
    const height = source.height | 0;

    if (source.data && (isTypedArray(source.data) || isDataView(source.data) || isArrayBuffer(source.data))) {
        return {
            width,
            height,
            data: toUnit8Array(source.data),
        };
    }

    if (typeof source.getContext === "function") {
        if (width <= 0 || height <= 0) return { width: 0, height: 0, data: EMPTY_UPLOAD_BYTES };
        try {
            const ctx2d = source.getContext("2d");
            if (ctx2d && typeof ctx2d.getImageData === "function") {
                const imgData = ctx2d.getImageData(0, 0, width, height);
                if (imgData && imgData.data) {
                    return {
                        width,
                        height,
                        data: toUnit8Array(imgData.data),
                    };
                }
            }
        } catch (_) {
            // fall through to unsupported warning
        }
    }

    return null;
}

function _loc(location) {
    return location !== null && location !== undefined ? location.id : -1;
}

function nextResourceId() {
    const id = op_alloc_gl_resource_id();
    if (id <= 0) {
        throw new Error("Failed to allocate WebGL resource id");
    }
    return id;
}

// A `sequence<GLenum>` argument (`drawBuffers`, `invalidateFramebuffer`) as the Uint32Array the op takes.
// A number is not a sequence: `new Uint32Array(4294967295)` is a 16 GiB allocation, which is what the bare
// constructor made of `invalidateFramebuffer(target, 4294967295)` -- the isolate stopped answering until the
// watchdog ended it. WebIDL says a value that is not an iterable object is a TypeError; null and undefined stay
// the empty list they were taken for, which is what content written against the old behaviour passes.
function toGLenumSequence(value) {
    if (value instanceof Uint32Array) return value;
    if (value === null || value === undefined) return new Uint32Array(0);
    if (typeof value !== "object" || typeof value[Symbol.iterator] !== "function") {
        throw new TypeError("Failed to convert value to 'sequence<GLenum>'");
    }
    return Uint32Array.from(value);
}

// A WebIDL `long long` argument (GLintptr, GLsizeiptr): the integer part, wrapped modulo 2^64 into the signed range,
// with NaN and the infinities 0. A BigInt or a Symbol is a TypeError, as WebIDL's ToNumber makes it.
function toLongLong(value) {
    const n = MathTrunc(+value);
    if (!NumberIsFinite(n)) return 0;
    if (n >= -0x8000000000000000 && n < 0x8000000000000000) return n;
    const r = n % 0x10000000000000000;
    return r >= 0x8000000000000000 ? r - 0x10000000000000000 : (r < -0x8000000000000000 ? r + 0x10000000000000000 : r);
}

// A WebIDL `unsigned long long` argument (a `srcOffset`): the integer part modulo 2^64, with NaN and the infinities 0.
// A negative value wraps to one at or past 2^63 -- past the end of any list, which is what an offset is compared
// with. `>>> 0` wrapped modulo 2^32 instead, so an offset of 2^32 read from the start of the list.
function toUnsignedLongLong(value) {
    const n = MathTrunc(+value);
    if (!NumberIsFinite(n)) return 0;
    const r = n % 0x10000000000000000;
    return r < 0 ? r + 0x10000000000000000 : r;
}

// A GLintptr offset into the bound PIXEL_UNPACK_BUFFER (WebGL 2's buffer overloads of the uploads): a `long long`
// that may not be negative (INVALID_VALUE). No buffer reaches 2^31 bytes -- the render side holds a buffer's size as a
// GLint -- so an offset past that reads past the buffer (INVALID_OPERATION, as a browser answers it), and what crosses
// is exact. -1 when refused, the error recorded.
function unpackBufferOffset(canvasId, value) {
    const n = toLongLong(value);
    if (n < 0 || n > 0x7fffffff) {
        recordGpuPreflightError(canvasId, n < 0 ? GL_INVALID_VALUE : GL_INVALID_OPERATION);
        return -1;
    }
    return n;
}

const EMPTY_UPLOAD_BYTES = new Uint8Array(0);

// The task a fence was made in. WebGL keeps a sync object unsignalled until control has left the task that made it,
// so a loop cannot spin on one inside a task (`clientWaitSync` and SYNC_STATUS answer from here without crossing). A
// frame is not that boundary in this runtime: the frame loop runs only while content has asked for a frame, and a
// fence polled from timers would never come due. The epoch advances in a microtask that the first fence of a task
// schedules -- once the task's own code has run -- and that microtask also flushes each context that made a fence, as
// a browser flushes at the end of every task: a fence that is never submitted never signals, and a context drawn into
// offscreen is submitted by nothing else.
let _syncTaskEpoch = 0;
let _syncEpochPending = false;
const _fencedCanvases = [];
function _endSyncTask() {
    _syncEpochPending = false;
    _syncTaskEpoch++;
    if (!op_gl_is_context_lost()) for (let k = 0; k < _fencedCanvases.length; k++) encodeFlush(_fencedCanvases[k]);
    _fencedCanvases.length = 0;
    _rawGlFlush();
}
function syncTaskEpoch(canvasId) {
    let known = false;
    for (let k = 0; k < _fencedCanvases.length; k++) if (_fencedCanvases[k] === canvasId) known = true;
    if (!known) _fencedCanvases[_fencedCanvases.length] = canvasId;
    if (!_syncEpochPending) {
        _syncEpochPending = true;
        PromisePrototypeThen(PromiseResolve(undefined), _endSyncTask);
    }
    return _syncTaskEpoch;
}

// The slot a query target's active query is kept in: the two occlusion targets share ANY_SAMPLES_PASSED's; undefined
// for a target that is not a query target (TIME_ELAPSED needs an extension this context does not offer).
function _querySlot(target) {
    if (target === 0x8c2f || target === 0x8d6a) return 0x8c2f;    // ANY_SAMPLES_PASSED, _CONSERVATIVE
    if (target === 0x8c88) return 0x8c88;                         // TRANSFORM_FEEDBACK_PRIMITIVES_WRITTEN
    return undefined;
}

// Membership without `Array.prototype.includes`, which content can replace.
function _listHas(list, value) {
    for (let k = 0; k < list.length; k++) if (list[k] === value) return true;
    return false;
}

// The error a name passed to a lookup by name is (WebGL 1.0 6.20, 6.21): INVALID_VALUE past `maxLength` (256 in
// WebGL 1, 1024 in WebGL 2) or for a character outside the GLSL ES source character set -- the printable ASCII but
// `"`, `$`, `'`, `@`, `\` and the backquote, and the whitespace controls TAB to CR -- or 0 for a name GL may be asked.
function _glslNameError(name, maxLength) {
    if (name.length > maxLength) return GL_INVALID_VALUE;
    for (let k = 0; k < name.length; k++) {
        const c = StringPrototypeCharCodeAt(name, k);
        if (c >= 9 && c <= 13) continue;
        if (c < 32 || c > 126 || c === 34 || c === 36 || c === 39 || c === 64 || c === 92 || c === 96) {
            return GL_INVALID_VALUE;
        }
    }
    return 0;
}

// A name WebGL reserves (`webgl_`, `_webgl_`): a lookup by it finds nothing, and binding it is INVALID_OPERATION.
function _isReservedGlslName(name) {
    return StringPrototypeStartsWith(name, "webgl_") || StringPrototypeStartsWith(name, "_webgl_");
}

// `getUniform`'s answer as WebGL types it, from the renderer's `[kind, words]` (`frame_wire::sync::gl_state::
// UNIFORM_VALUE`): a scalar is a number or a boolean; a vector or a matrix a Float32Array, an Int32Array, a
// Uint32Array or, of booleans, an Array. A float travels as its bits, so it is the float the uniform holds.
function _uniformValue(kind, words) {
    const count = words.length;
    if (kind === "b") {
        if (count === 1) return words[0] !== 0;
        const flags = [];
        for (let k = 0; k < count; k++) flags[k] = words[k] !== 0;
        return flags;
    }
    const bits = new Uint32Array(count);
    for (let k = 0; k < count; k++) bits[k] = words[k];
    if (kind === "u") return count === 1 ? bits[0] : bits;
    const typed = kind === "f" ? new Float32Array(bits.buffer) : new Int32Array(bits.buffer);
    return count === 1 ? typed[0] : typed;
}

// A sampler's parameters (ES 3.0 table 6.10): what each is until set, and the values an enum one may take; MIN_LOD and
// MAX_LOD take any float. `getSamplerParameter` answers from what `samplerParameter*` set, which is why a value
// outside these is refused here (INVALID_ENUM) rather than left to the driver: the answer would otherwise be a value
// the sampler does not have.
const _WRAP_MODES = [0x2901, 0x812f, 0x8370];                       // REPEAT, CLAMP_TO_EDGE, MIRRORED_REPEAT
const _SAMPLER_PARAMETERS = new Map([
    [0x2801, { initial: 0x2702, values: [0x2600, 0x2601, 0x2700, 0x2701, 0x2702, 0x2703] }],   // MIN_FILTER
    [0x2800, { initial: 0x2601, values: [0x2600, 0x2601] }],                                   // MAG_FILTER
    [0x2802, { initial: 0x2901, values: _WRAP_MODES }],                                        // WRAP_S
    [0x2803, { initial: 0x2901, values: _WRAP_MODES }],                                        // WRAP_T
    [0x8072, { initial: 0x2901, values: _WRAP_MODES }],                                        // WRAP_R
    [0x884c, { initial: 0, values: [0, 0x884e] }],                                             // COMPARE_MODE: NONE, COMPARE_REF_TO_TEXTURE
    [0x884d, { initial: 0x0203, values: [0x0200, 0x0201, 0x0202, 0x0203, 0x0204, 0x0205, 0x0206, 0x0207] }],   // COMPARE_FUNC
    [0x813a, { initial: -1000, values: null }],                                                // TEXTURE_MIN_LOD
    [0x813b, { initial: 1000, values: null }],                                                 // TEXTURE_MAX_LOD
]);
// A texture's parameters (ES 3.0 table 6.13): its sampler's, and the levels it is sampled from, any integer not below 0
// (a negative one is INVALID_VALUE). WebGL 1 has the filters and the two wraps only. `getTexParameter` answers from what
// `texParameter*` set, so a value outside these is refused here, as a sampler's is.
const _TEXTURE_PARAMETERS = new Map(_SAMPLER_PARAMETERS);
_TEXTURE_PARAMETERS.set(0x813c, { initial: 0, values: null });         // TEXTURE_BASE_LEVEL
_TEXTURE_PARAMETERS.set(0x813d, { initial: 1000, values: null });      // TEXTURE_MAX_LEVEL
const _WEBGL1_TEXTURE_PARAMETERS = [0x2800, 0x2801, 0x2802, 0x2803];  // MAG_FILTER, MIN_FILTER, WRAP_S, WRAP_T
// TEXTURE_MAX_ANISOTROPY_EXT, a texture's and a sampler's once EXT_texture_filter_anisotropic is enabled: a float from 1
// to MAX_TEXTURE_MAX_ANISOTROPY_EXT.
const _ANISOTROPY_PARAMETER = { initial: 1, values: null };

// The bytes of WebGL 2's `offset` / `length` pair over `view`, both counted in its elements (a DataView's are
// bytes): `length` elements from `offset`, or the rest when it is 0, as a Uint8Array over the view's memory. A range
// past the view is INVALID_VALUE and null.
function viewElementRange(canvasId, view, offset, length) {
    const dataView = isDataView(view);
    const unit = dataView ? 1 : view.BYTES_PER_ELEMENT;
    const byteLength = dataView ? DataViewPrototypeGetByteLength(view) : TypedArrayPrototypeGetByteLength(view);
    const byteOffset = dataView ? DataViewPrototypeGetByteOffset(view) : TypedArrayPrototypeGetByteOffset(view);
    const buffer = dataView ? DataViewPrototypeGetBuffer(view) : TypedArrayPrototypeGetBuffer(view);
    const elements = byteLength / unit;
    const first = toUnsignedLongLong(offset);
    const count = length >>> 0;
    if (first > elements || (count !== 0 && first + count > elements)) {
        recordGpuPreflightError(canvasId, GL_INVALID_VALUE);
        return null;
    }
    const taken = count !== 0 ? count : elements - first;
    return new Uint8Array(buffer, byteOffset + first * unit, taken * unit);
}

// `viewElementRange` as the source of an upload: also null past the upload budget (`toBoundedUploadBytes`).
function viewElementBytes(canvasId, view, srcOffset, length) {
    const range = viewElementRange(canvasId, view, srcOffset, length);
    return range === null ? null : toBoundedUploadBytes(canvasId, range);
}

// Channel sizes in bits of a renderbuffer format: red, green, blue, alpha, depth, stencil (`_FORMAT_INFO`).
function _renderbufferBits(format, channel) {
    const info = _FORMAT_INFO.get(format);
    return info === undefined ? 0 : info[channel];
}

// FRAMEBUFFER_ATTACHMENT_COMPONENT_TYPE of a format: the type its components are read as.
function _componentType(format) {
    const info = _FORMAT_INFO.get(format);
    if (info === undefined) return 0x8c17;                         // UNSIGNED_NORMALIZED
    return [0x8c17, 0x8f9c, 0x1406, 0x1404, 0x1405][info[6]];      // UNSIGNED_/SIGNED_NORMALIZED, FLOAT, INT, UNSIGNED_INT
}

class WebglObject {
    // `kind` is what `isTexture` and its siblings tell the objects apart by, `ownerId` the canvas of the context
    // that made it (a number, so an object still serialises).
    constructor(id, kind, ownerId) {
        this._id = id;
        this._kind = kind;
        this._ownerId = ownerId;
        this._deleted = false;
    }

    get id() {
        return this._id;
    }
}

// WebIDL brand check before any GL work. `dstData` is an ArrayBufferView; only
// WebGL 1 accepts null, which the native side reports as INVALID_VALUE. A view
// of the wrong element type is not a TypeError -- readPixels reports that as
// INVALID_OPERATION -- so a DataView reaches the native check like any view.
function checkReadPixelsDestination(pixels, nullable) {
    if (pixels === null || pixels === undefined ? nullable : ArrayBufferIsView(pixels)) {
        return;
    }
    throw new TypeError("readPixels: dstData must be an ArrayBufferView");
}

// INVALID_OPERATION for a readPixels view of another type than `type` writes (`_UPLOAD_VIEWS`), else 0.
function _readViewError(view, type) {
    const kind = _UPLOAD_VIEWS.get(type);
    const tag = TypedArrayPrototypeGetSymbolToStringTag(view);
    return kind !== undefined && (tag === kind[0] || (type === _UBYTE && tag === "Uint8ClampedArray"))
        ? 0 : GL_INVALID_OPERATION;
}

// The native result already includes the validated destination byte offset.
// Reuse the original view and compact payload; an offset needs no subarray or
// additional pixel storage.
function readPixelsIntoView(canvasId, x, y, width, height, format, type, pixels, dstOffset) {
    const result = _rawReadPixels(canvasId, x, y, width, height, format, type, pixels, dstOffset);
    if (!result || TypedArrayPrototypeGetByteLength(result.data) === 0) return;
    const u8 = new Uint8Array(
        TypedArrayPrototypeGetBuffer(pixels), TypedArrayPrototypeGetByteOffset(pixels),
        TypedArrayPrototypeGetByteLength(pixels),
    );
    const { data, firstByte, rowBytes, rowStride, height: rows } = result;
    if (rowStride === rowBytes) {
        TypedArrayPrototypeSet(u8, data, firstByte);
    } else {
        const source = TypedArrayPrototypeGetBuffer(data);
        const sourceOffset = TypedArrayPrototypeGetByteOffset(data);
        for (let row = 0; row < rows; ++row) {
            TypedArrayPrototypeSet(u8,
                new Uint8Array(source, sourceOffset + row * rowBytes, rowBytes),
                firstByte + row * rowStride);
        }
    }
}

// What `getVertexAttrib` answers: the array state of each attribute, kept per vertex array object, since the render
// side holds the real state and a query for it would cross for a value this side wrote itself. 32 is a ceiling on
// MAX_VERTEX_ATTRIBS (16 on every device the engine runs on); an index past it is not shadowed, and
// `getVertexAttrib` refuses it against the real limit.
const _ATTRIB_SHADOW_SLOTS = 32;
class VertexAttribShadow {
    constructor() {
        this.enabled = new Uint8Array(_ATTRIB_SHADOW_SLOTS);
        this.size = new Int32Array(_ATTRIB_SHADOW_SLOTS).fill(4);
        this.type = new Uint32Array(_ATTRIB_SHADOW_SLOTS).fill(0x1406);   // FLOAT
        this.normalized = new Uint8Array(_ATTRIB_SHADOW_SLOTS);
        this.integer = new Uint8Array(_ATTRIB_SHADOW_SLOTS);
        this.stride = new Int32Array(_ATTRIB_SHADOW_SLOTS);
        this.offset = new Float64Array(_ATTRIB_SHADOW_SLOTS);
        this.divisor = new Uint32Array(_ATTRIB_SHADOW_SLOTS);
        this.buffer = new Array(_ATTRIB_SHADOW_SLOTS).fill(null);
        // ELEMENT_ARRAY_BUFFER is vertex array object state (ES 3.0 table 6.2): a draw reads the indices of the
        // object bound, and binding another object binds its own.
        this.elementArrayBuffer = null;
    }
}

// Whether an attribute array is enabled at divisor 0, at any location.
function _hasEnabledDivisorZero(shadow) {
    for (let k = 0; k < _ATTRIB_SHADOW_SLOTS; k++) if (shadow.enabled[k] !== 0 && shadow.divisor[k] === 0) return true;
    return false;
}

// The arguments `vertexAttribPointer` / `vertexAttribIPointer` accept: what the host's decoder checks, so that the
// shadow holds what the render side took and not what a refused call asked for.
function _attribPointerAccepted(size, type, stride, offset, integer) {
    if (!(size >= 1 && size <= 4)) return false;
    if (!(stride >= 0 && stride <= 255) || !(offset >= 0)) return false;
    switch (type) {
        case 0x1400: case 0x1401: case 0x1402: case 0x1403: case 0x1404: case 0x1405: return true;
        case 0x1406: case 0x140b: return !integer;   // FLOAT, HALF_FLOAT
        default: return false;
    }
}

// `texImage2D`'s 9-argument forms with bytes or a buffer offset once the call's own checks have passed: true when the
// upload was sent. At module scope, as `defineTextureImage` is: nothing reachable from content sends an upload its
// checks did not pass.
function texImageFromData(ctx, target, level, internalformat, width, height, border, format, type, pixels, srcOffset, fromBuffer) {
    if (fromBuffer) {
        const offset = ctx._unpackBufferOffset(pixels, width, height, 1, format, type, false);
        if (offset < 0) return false;
        _rawTexImage2D(ctx._canvasId, target, level, internalformat, width, height, border, format, type, null, offset);
        return true;
    }
    if (pixels == null) {
        _rawTexImage2D(ctx._canvasId, target, level, internalformat, width, height, border, format, type, null, -1);
        return true;
    }
    if (ctx._refusesUnpackRegion(width, height, false)) return false;
    const data = ctx._uploadViewBytes(pixels, ctx._isWebGL2() ? srcOffset : 0, width, height, 1, format, type, false);
    if (data === null) return false;
    _rawTexImage2D(ctx._canvasId, target, level, internalformat, width, height, border, format, type, data, -1);
    return true;
}

// ---- Extensions -------------------------------------------------------------------------------------------------------
//
// Every extension a context offers, as the Khronos registry names it: the WebGL versions that have it -- a WebGL 1
// extension that is WebGL 2 core is not WebGL 2's --, the renderer capabilities it needs (`_gpuCaps` bits, every one
// of them), the context field that holds its object once it is enabled (what the rest of the facade reads), and how the
// object is made. `getExtension` and `getSupportedExtensions` both read this, so what is listed is what is answered.
const _WEBGL1_ONLY = 1, _WEBGL2_ONLY = 2, _ANY_WEBGL = 3;
const _EXTENSIONS = [
    // Instanced drawing: the ops are ES 3.0's own. Cocos Creator 2.x's particles and any instancing sprite batcher go
    // from a draw call per instance to one.
    ["ANGLE_instanced_arrays", _WEBGL1_ONLY, 0, "_angleInstancedArrays", (ctx) => ctx._buildAngleInstancedArrays()],
    // WebGL 1's MIN and MAX blend equations, ES 3.0 core.
    ["EXT_blend_minmax", _WEBGL1_ONLY, 0, "_extBlendMinmax", () => ({ MIN_EXT: 0x8007, MAX_EXT: 0x8008 })],
    // The float formats as colour attachments and renderbuffers, where the driver renders to them.
    ["EXT_color_buffer_float", _WEBGL2_ONLY, 4, "_extColorBufferFloat", enableColourBuffers],
    // The 16-bit float formats alone, where the driver renders to those: WebGL 2's R16F, RG16F and RGBA16F, WebGL 1's
    // half-float textures and RGBA16F_EXT / RGB16F_EXT renderbuffers.
    ["EXT_color_buffer_half_float", _ANY_WEBGL, 32, "_extColorBufferHalfFloat", (ctx) => enableColourBuffers(ctx, {
        RGBA16F_EXT: 0x881a, RGB16F_EXT: 0x881b, FRAMEBUFFER_ATTACHMENT_COMPONENT_TYPE_EXT: 0x8211,
        UNSIGNED_NORMALIZED_EXT: 0x8c17,
    })],
    // Blending into 32-bit floats, where the driver does: with EXT_color_buffer_float, asked for or not, as a browser
    // enables it implicitly (`_drawError`).
    ["EXT_float_blend", _ANY_WEBGL, 8, "_extFloatBlend", () => ({})],
    // gl_FragDepthEXT in a WebGL 1 fragment shader, where the driver compiles ESSL 1.00's GL_EXT_frag_depth.
    ["EXT_frag_depth", _WEBGL1_ONLY, 512, "_extFragDepth", () => ({})],
    // texture2DLodEXT and its kin in WebGL 1 shaders, where the driver compiles GL_EXT_shader_texture_lod.
    ["EXT_shader_texture_lod", _WEBGL1_ONLY, 256, "_extShaderTextureLod", () => ({})],
    // WebGL 1's sRGB textures and renderbuffers, ES 3.0's SRGB8 and SRGB8_ALPHA8.
    ["EXT_sRGB", _WEBGL1_ONLY, 0, "_extSrgb", () => ({
        SRGB_EXT: 0x8c40, SRGB_ALPHA_EXT: 0x8c42, SRGB8_ALPHA8_EXT: 0x8c43, FRAMEBUFFER_ATTACHMENT_COLOR_ENCODING_EXT: 0x8210,
    })],
    ["EXT_texture_filter_anisotropic", _ANY_WEBGL, 16, "_extTextureFilterAnisotropic",
        () => ({ TEXTURE_MAX_ANISOTROPY_EXT: 0x84fe, MAX_TEXTURE_MAX_ANISOTROPY_EXT: 0x84ff })],
    // 32-bit element indices, ES 3.0 core: without it Pixi and three.js cap batches at 65535 indices.
    ["OES_element_index_uint", _WEBGL1_ONLY, 0, "_oesElementIndexUint", () => ({})],
    // A WebGL 1 framebuffer attachment of a level other than 0, ES 3.0 core.
    ["OES_fbo_render_mipmap", _WEBGL1_ONLY, 0, "_oesFboRenderMipmap", () => ({})],
    // dFdx, dFdy and fwidth in WebGL 1 fragment shaders, where the driver compiles GL_OES_standard_derivatives.
    ["OES_standard_derivatives", _WEBGL1_ONLY, 128, "_oesStandardDerivatives", () => ({ FRAGMENT_SHADER_DERIVATIVE_HINT_OES: 0x8b8b })],
    // WebGL 1's float textures, uploaded as ES 3.0's 32-bit float ones; WEBGL_color_buffer_float with them where the
    // renderer has it, as a browser enables it implicitly.
    ["OES_texture_float", _WEBGL1_ONLY, 0, "_oesTextureFloat", (ctx) => enableFloatTextures(ctx, 4, "_webglColorBufferFloat",
        () => webglColorBufferFloat(ctx), {})],
    // Linear filtering of 32-bit float textures, where the driver filters them: without it such a texture is incomplete
    // while a filter of it is not NEAREST (`samplesBlack`).
    ["OES_texture_float_linear", _ANY_WEBGL, 64, "_oesTextureFloatLinear", enableFloatFiltering],
    // WebGL 1's half-float textures (HALF_FLOAT_OES), uploaded as ES 3.0's 16-bit float ones; EXT_color_buffer_half_float
    // with them where the renderer has it, as a browser enables it implicitly.
    ["OES_texture_half_float", _WEBGL1_ONLY, 0, "_oesTextureHalfFloat", (ctx) => enableFloatTextures(ctx, 32,
        "_extColorBufferHalfFloat", () => enableColourBuffers(ctx, {
            RGBA16F_EXT: 0x881a, RGB16F_EXT: 0x881b, FRAMEBUFFER_ATTACHMENT_COMPONENT_TYPE_EXT: 0x8211,
            UNSIGNED_NORMALIZED_EXT: 0x8c17,
        }), { HALF_FLOAT_OES: 0x8d61 })],
    // Linear filtering of WebGL 1's half-float textures, which ES 3.0 filters: without it such a texture is incomplete
    // while a filter of it is not NEAREST (`samplesBlack`).
    ["OES_texture_half_float_linear", _WEBGL1_ONLY, 0, "_oesTextureHalfFloatLinear", enableFloatFiltering],
    // Vertex array objects, ES 3.0 core: Cocos Creator 2.x falls back to a vertexAttribPointer storm per draw without.
    ["OES_vertex_array_object", _WEBGL1_ONLY, 0, "_oesVertexArrayObject", (ctx) => ctx._buildOesVertexArrayObject()],
    // Compressed uploads: ETC2/EAC where the driver decodes them (ES 3.0 core), ETC1 as their subset, ASTC where it has
    // GL_KHR_texture_compression_astc_*. A compressed asset saves ~16 MiB of heap per 2048^2 texture over RGBA.
    // WebGL 1's RGBA32F renderbuffers and float textures as colour attachments, where the driver renders to them.
    ["WEBGL_color_buffer_float", _WEBGL1_ONLY, 4, "_webglColorBufferFloat", (ctx) => webglColorBufferFloat(ctx)],
    ["WEBGL_compressed_texture_astc", _ANY_WEBGL, 2, "_webglCompressedAstc", (ctx) => ctx._buildCompressedAstc()],
    ["WEBGL_compressed_texture_etc", _ANY_WEBGL, 1, "_webglCompressedEtc", (ctx) => ctx._buildCompressedEtc()],
    ["WEBGL_compressed_texture_etc1", _ANY_WEBGL, 1, "_webglCompressedEtc1", () => ({ COMPRESSED_RGB_ETC1_WEBGL: 0x8d64 })],
    // The driver's own vendor and renderer strings, where VENDOR and RENDERER answer a browser's masked ones. Answered
    // whether or not this is enabled, as browsers now answer them.
    ["WEBGL_debug_renderer_info", _ANY_WEBGL, 0, "_webglDebugRendererInfo",
        () => ({ UNMASKED_VENDOR_WEBGL: 0x9245, UNMASKED_RENDERER_WEBGL: 0x9246 })],
    // WebGL 1's depth and depth-stencil textures: ES 3.0's DEPTH_COMPONENT16 / 24 and DEPTH24_STENCIL8, a 2D level 0
    // without data, never sub-uploaded, copied or mipmapped.
    ["WEBGL_depth_texture", _WEBGL1_ONLY, 0, "_webglDepthTexture", () => ({ UNSIGNED_INT_24_8_WEBGL: 0x84fa })],
    // Multiple render targets: WebGL 2's drawBuffers under WebGL 1's names.
    ["WEBGL_draw_buffers", _WEBGL1_ONLY, 0, "_webglDrawBuffers", (ctx) => ctx._buildWebglDrawBuffers()],
    // Loses THIS context, as the extension specifies: its isContextLost() turns true and its canvas is sent
    // webglcontextlost; every other context -- the game's own among them -- is untouched. Engines lose a probe context
    // with it (Pixi does, twice, while choosing a renderer); the probe's GPU objects go with its canvas.
    ["WEBGL_lose_context", _ANY_WEBGL, 0, "_webglLoseContext", (ctx) => ({
        loseContext: () => { ctx._setLostByExtension(true); },
        restoreContext: () => { ctx._setLostByExtension(false); },
    })],
];
// ---- getParameter -----------------------------------------------------------------------------------------------------
// The parameters getParameter asks the driver for (WebGL 1.0 5.14.3, WebGL 2.0 3.7.2), each with the versions that
// have it and the type WebGL gives its answer: a number, a boolean, a Float32Array, an Int32Array, or booleans. The
// rest of what it answers -- the bindings, the pixel-store state, the capabilities, the strings, the stencil masks,
// the compressed formats, the extensions' -- is this facade's own. A parameter of neither, or of the other version,
// is INVALID_ENUM and null.
const _P_NUMBER = 0, _P_BOOLEAN = 1, _P_FLOAT32 = 2, _P_INT32 = 3, _P_BOOLEANS = 4;
const _DRIVER_PARAMETERS = new Map([
    [0x0b21, [_ANY_WEBGL, _P_NUMBER]],      // LINE_WIDTH
    [0x0b45, [_ANY_WEBGL, _P_NUMBER]],      // CULL_FACE_MODE
    [0x0b46, [_ANY_WEBGL, _P_NUMBER]],      // FRONT_FACE
    [0x0b70, [_ANY_WEBGL, _P_FLOAT32]],     // DEPTH_RANGE
    [0x0b72, [_ANY_WEBGL, _P_BOOLEAN]],     // DEPTH_WRITEMASK
    [0x0b73, [_ANY_WEBGL, _P_NUMBER]],      // DEPTH_CLEAR_VALUE
    [0x0b74, [_ANY_WEBGL, _P_NUMBER]],      // DEPTH_FUNC
    [0x0b91, [_ANY_WEBGL, _P_NUMBER]],      // STENCIL_CLEAR_VALUE
    [0x0b92, [_ANY_WEBGL, _P_NUMBER]],      // STENCIL_FUNC
    [0x0b94, [_ANY_WEBGL, _P_NUMBER]],      // STENCIL_FAIL
    [0x0b95, [_ANY_WEBGL, _P_NUMBER]],      // STENCIL_PASS_DEPTH_FAIL
    [0x0b96, [_ANY_WEBGL, _P_NUMBER]],      // STENCIL_PASS_DEPTH_PASS
    [0x0b97, [_ANY_WEBGL, _P_NUMBER]],      // STENCIL_REF
    [0x0ba2, [_ANY_WEBGL, _P_INT32]],       // VIEWPORT
    [0x0c10, [_ANY_WEBGL, _P_INT32]],       // SCISSOR_BOX
    [0x0c22, [_ANY_WEBGL, _P_FLOAT32]],     // COLOR_CLEAR_VALUE
    [0x0c23, [_ANY_WEBGL, _P_BOOLEANS]],    // COLOR_WRITEMASK
    [0x0d33, [_ANY_WEBGL, _P_NUMBER]],      // MAX_TEXTURE_SIZE
    [0x0d3a, [_ANY_WEBGL, _P_INT32]],       // MAX_VIEWPORT_DIMS
    [0x0d50, [_ANY_WEBGL, _P_NUMBER]],      // SUBPIXEL_BITS
    [0x0d52, [_ANY_WEBGL, _P_NUMBER]],      // RED_BITS
    [0x0d53, [_ANY_WEBGL, _P_NUMBER]],      // GREEN_BITS
    [0x0d54, [_ANY_WEBGL, _P_NUMBER]],      // BLUE_BITS
    [0x0d55, [_ANY_WEBGL, _P_NUMBER]],      // ALPHA_BITS
    [0x0d56, [_ANY_WEBGL, _P_NUMBER]],      // DEPTH_BITS
    [0x0d57, [_ANY_WEBGL, _P_NUMBER]],      // STENCIL_BITS
    [0x2a00, [_ANY_WEBGL, _P_NUMBER]],      // POLYGON_OFFSET_UNITS
    [0x8005, [_ANY_WEBGL, _P_FLOAT32]],     // BLEND_COLOR
    [0x8009, [_ANY_WEBGL, _P_NUMBER]],      // BLEND_EQUATION (_RGB)
    [0x8038, [_ANY_WEBGL, _P_NUMBER]],      // POLYGON_OFFSET_FACTOR
    [0x8073, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_3D_TEXTURE_SIZE
    [0x80a8, [_ANY_WEBGL, _P_NUMBER]],      // SAMPLE_BUFFERS
    [0x80a9, [_ANY_WEBGL, _P_NUMBER]],      // SAMPLES
    [0x80aa, [_ANY_WEBGL, _P_NUMBER]],      // SAMPLE_COVERAGE_VALUE
    [0x80ab, [_ANY_WEBGL, _P_BOOLEAN]],     // SAMPLE_COVERAGE_INVERT
    [0x80c8, [_ANY_WEBGL, _P_NUMBER]],      // BLEND_DST_RGB
    [0x80c9, [_ANY_WEBGL, _P_NUMBER]],      // BLEND_SRC_RGB
    [0x80ca, [_ANY_WEBGL, _P_NUMBER]],      // BLEND_DST_ALPHA
    [0x80cb, [_ANY_WEBGL, _P_NUMBER]],      // BLEND_SRC_ALPHA
    [0x80e8, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_ELEMENTS_VERTICES
    [0x80e9, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_ELEMENTS_INDICES
    [0x8192, [_ANY_WEBGL, _P_NUMBER]],      // GENERATE_MIPMAP_HINT
    [0x846d, [_ANY_WEBGL, _P_FLOAT32]],     // ALIASED_POINT_SIZE_RANGE
    [0x846e, [_ANY_WEBGL, _P_FLOAT32]],     // ALIASED_LINE_WIDTH_RANGE
    [0x84e0, [_ANY_WEBGL, _P_NUMBER]],      // ACTIVE_TEXTURE
    [0x84e8, [_ANY_WEBGL, _P_NUMBER]],      // MAX_RENDERBUFFER_SIZE
    [0x84fd, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_TEXTURE_LOD_BIAS
    [0x851c, [_ANY_WEBGL, _P_NUMBER]],      // MAX_CUBE_MAP_TEXTURE_SIZE
    [0x8800, [_ANY_WEBGL, _P_NUMBER]],      // STENCIL_BACK_FUNC
    [0x8801, [_ANY_WEBGL, _P_NUMBER]],      // STENCIL_BACK_FAIL
    [0x8802, [_ANY_WEBGL, _P_NUMBER]],      // STENCIL_BACK_PASS_DEPTH_FAIL
    [0x8803, [_ANY_WEBGL, _P_NUMBER]],      // STENCIL_BACK_PASS_DEPTH_PASS
    [0x8824, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_DRAW_BUFFERS (WEBGL_draw_buffers' in WebGL 1)
    [0x883d, [_ANY_WEBGL, _P_NUMBER]],      // BLEND_EQUATION_ALPHA
    [0x8869, [_ANY_WEBGL, _P_NUMBER]],      // MAX_VERTEX_ATTRIBS
    [0x8872, [_ANY_WEBGL, _P_NUMBER]],      // MAX_TEXTURE_IMAGE_UNITS
    [0x88ff, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_ARRAY_TEXTURE_LAYERS
    [0x8904, [_WEBGL2_ONLY, _P_NUMBER]],    // MIN_PROGRAM_TEXEL_OFFSET
    [0x8905, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_PROGRAM_TEXEL_OFFSET
    [0x8a2b, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_VERTEX_UNIFORM_BLOCKS
    [0x8a2d, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_FRAGMENT_UNIFORM_BLOCKS
    [0x8a2e, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_COMBINED_UNIFORM_BLOCKS
    [0x8a2f, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_UNIFORM_BUFFER_BINDINGS
    [0x8a30, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_UNIFORM_BLOCK_SIZE (GLint64)
    [0x8a31, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_COMBINED_VERTEX_UNIFORM_COMPONENTS (GLint64)
    [0x8a33, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_COMBINED_FRAGMENT_UNIFORM_COMPONENTS (GLint64)
    [0x8a34, [_WEBGL2_ONLY, _P_NUMBER]],    // UNIFORM_BUFFER_OFFSET_ALIGNMENT
    [0x8b49, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_FRAGMENT_UNIFORM_COMPONENTS
    [0x8b4a, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_VERTEX_UNIFORM_COMPONENTS
    [0x8b4b, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_VARYING_COMPONENTS
    [0x8b4c, [_ANY_WEBGL, _P_NUMBER]],      // MAX_VERTEX_TEXTURE_IMAGE_UNITS
    [0x8b4d, [_ANY_WEBGL, _P_NUMBER]],      // MAX_COMBINED_TEXTURE_IMAGE_UNITS
    [0x8c80, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_TRANSFORM_FEEDBACK_SEPARATE_COMPONENTS
    [0x8c8a, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_TRANSFORM_FEEDBACK_INTERLEAVED_COMPONENTS
    [0x8c8b, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_TRANSFORM_FEEDBACK_SEPARATE_ATTRIBS
    [0x8ca3, [_ANY_WEBGL, _P_NUMBER]],      // STENCIL_BACK_REF
    [0x8cdf, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_COLOR_ATTACHMENTS (WEBGL_draw_buffers' in WebGL 1)
    [0x8d57, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_SAMPLES
    [0x8d6b, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_ELEMENT_INDEX (GLint64)
    [0x8dfb, [_ANY_WEBGL, _P_NUMBER]],      // MAX_VERTEX_UNIFORM_VECTORS
    [0x8dfc, [_ANY_WEBGL, _P_NUMBER]],      // MAX_VARYING_VECTORS
    [0x8dfd, [_ANY_WEBGL, _P_NUMBER]],      // MAX_FRAGMENT_UNIFORM_VECTORS
    [0x8e23, [_WEBGL2_ONLY, _P_BOOLEAN]],   // TRANSFORM_FEEDBACK_PAUSED
    [0x8e24, [_WEBGL2_ONLY, _P_BOOLEAN]],   // TRANSFORM_FEEDBACK_ACTIVE
    [0x9111, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_SERVER_WAIT_TIMEOUT (GLint64)
    [0x9122, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_VERTEX_OUTPUT_COMPONENTS
    [0x9125, [_WEBGL2_ONLY, _P_NUMBER]],    // MAX_FRAGMENT_INPUT_COMPONENTS
]);

// The driver's answer (JSON) as WebGL types it, or null for none.
function _typedParameter(json, kind) {
    let value = null;
    try { value = JSON.parse(json); } catch (_) { return null; }
    switch (kind) {
        case _P_BOOLEAN: return typeof value === "boolean" ? value : typeof value === "number" ? value !== 0 : null;
        case _P_FLOAT32: return ArrayIsArray(value) ? new Float32Array(value) : null;
        case _P_INT32: return ArrayIsArray(value) ? new Int32Array(value) : null;
        case _P_BOOLEANS: return ArrayIsArray(value) ? [value[0] === true, value[1] === true, value[2] === true, value[3] === true]
            : null;
        default: return typeof value === "number" ? value : null;
    }
}

// The stencil masks as set, which getParameter answers -- WebGL's are GLuint, which a driver answering GLint may
// clamp: the value masks of the front and back faces, then their write masks, all ones until set. A call the decoder
// refuses (`validate_stencil_func`, `validate_stencil_mask_separate`: a face that is not FRONT, BACK or
// FRONT_AND_BACK, a function that is no comparison) leaves them as they were. A rebuilt GL context is at its initial
// state, so its masks are all ones again.
function stencilMasksOf(ctx) {
    if (ctx._stencilGeneration !== _capGeneration) {
        for (let k = 0; k < 4; k++) ctx._stencilMasks[k] = 0xffffffff;
        ctx._stencilGeneration = _capGeneration;
    }
    return ctx._stencilMasks;
}
// Whether `func` is a comparison function, NEVER .. ALWAYS, as the decoder judges it.
function _isComparison(func) {
    const f = Number(func) >>> 0;
    return f >= 0x0200 && f <= 0x0207;
}
// `mask` for the faces `face` names, at `front` (the front face's index) and the back face's after it.
function noteStencilMask(ctx, face, front, mask) {
    const f = Number(face) >>> 0;
    if (f !== 0x0404 && f !== 0x0405 && f !== 0x0408) return;
    const masks = stencilMasksOf(ctx);
    const m = Number(mask) >>> 0;
    if (f !== 0x0405) masks[front] = m;
    if (f !== 0x0404) masks[front + 1] = m;
}

const _EXTENSIONS_BY_KEY = new Map();
for (const extension of _EXTENSIONS) _EXTENSIONS_BY_KEY.set(StringPrototypeToLowerCase(extension[0]), extension);

// Whether `ctx` offers `extension`: one of its WebGL version's, on a renderer with every capability it needs.
function offersExtension(ctx, extension) {
    const versions = extension[1], needs = extension[2];
    return (versions & (ctx._isWebGL2() ? _WEBGL2_ONLY : _WEBGL1_ONLY)) !== 0 && (needs === 0 || (ctx._gpuCaps & needs) === needs);
}

// OES_texture_float_linear or OES_texture_half_float_linear enabled: a float texture that sampled as incomplete may
// not now. The table stores the object in its field; it is set here first so the judgement sees it.
function enableFloatFiltering(ctx) {
    const extension = {};
    ctx[ctx._enabling] = extension;
    refreshAllSampling(ctx);
    return extension;
}

// OES_texture_float or OES_texture_half_float enabled, and with it the colour-buffer extension in `field` where the
// renderer has `caps`, made by `make`.
function enableFloatTextures(ctx, caps, field, make, extension) {
    if ((ctx._gpuCaps & caps) === caps && ctx[field] === undefined) ctx[field] = make();
    return extension;
}

// WEBGL_color_buffer_float enabled: RGBA32F is colour-renderable, so every framebuffer is judged again.
function webglColorBufferFloat() {
    framebufferChanged();
    return { RGBA32F_EXT: 0x8814, FRAMEBUFFER_ATTACHMENT_COMPONENT_TYPE_EXT: 0x8211, UNSIGNED_NORMALIZED_EXT: 0x8c17 };
}

// EXT_color_buffer_float or EXT_color_buffer_half_float enabled: a float attachment that was not colour-renderable may
// be now, so every framebuffer is judged again. `constants` are WebGL 1's names for what it adds (WebGL 2 has them).
function enableColourBuffers(ctx, constants) {
    framebufferChanged();
    return ctx === undefined || ctx._isWebGL2() || constants === undefined ? {} : constants;
}

// `drawBuffers` / `drawBuffersWEBGL`: refused as `_drawBuffersError` says, else recorded for the draw framebuffer --
// what DRAW_BUFFERi answers and a float-blend judgement reads (`drawsIntoFloat32`) -- and sent. Buffers past the list
// are NONE. The renderer turns BACK into the colour attachment of the FBO that stands in for the default framebuffer.
function drawBuffersOf(ctx, buffers) {
    // A copy: the content's own array may change after the call.
    const sequence = toGLenumSequence(buffers);
    const list = new Array(sequence.length);
    for (let i = 0; i < sequence.length; i++) list[i] = sequence[i];
    const error = ctx._drawBuffersError(list);
    if (error !== 0) {
        recordGpuPreflightError(ctx._canvasId, error);
        return;
    }
    const fb = ctx._framebufferBinding;
    if (fb === null) {
        ctx._defaultDrawBuffer = list[0];
    } else {
        fb._drawBuffers = list;
        fb._float32Generation = -1;
    }
    _rawDrawBuffers(ctx._canvasId, sequence);
}

// Whether an active draw buffer of the framebuffer object `fb` holds a 32-bit float image -- what blending needs
// EXT_float_blend for. Judged once a change of the framebuffers (`_framebufferGeneration`) or of `fb`'s draw buffers.
function drawsIntoFloat32(ctx, fb) {
    if (fb._float32Generation === _framebufferGeneration) return fb._float32;
    const buffers = fb._drawBuffers;
    const n = buffers === undefined ? 1 : buffers.length;
    let found = false;
    for (let i = 0; i < n && !found; i++) {
        const point = buffers === undefined ? 0x8ce0 : buffers[i];
        if (point === 0) continue;
        const record = fb._attachments ? fb._attachments.get(point) : undefined;
        const image = record ? _attachmentImage(ctx, record) : undefined;
        found = image !== undefined && (image[0] === 0x822e || image[0] === 0x8230 || image[0] === 0x8814);
    }
    fb._float32 = found;
    fb._float32Generation = _framebufferGeneration;
    return found;
}

// ---- TexImageSource uploads ----------------------------------------------------------------------------------------
//
// A TexImageSource -- a decoded image or ImageBitmap, a canvas, a snapshot of a 2D canvas (`getImageData`'s), other
// `ImageData` -- reaches the renderer as one upload (`uploadTexImageSource`), whatever the call, which converts its
// pixels as the call asks: the pixels UNPACK_SKIP_PIXELS / UNPACK_SKIP_ROWS select (and in 3D UNPACK_IMAGE_HEIGHT /
// UNPACK_SKIP_IMAGES), in the order UNPACK_FLIP_Y_WEBGL asks, with the alpha UNPACK_PREMULTIPLY_ALPHA_WEBGL asks, packed
// as the call's format and type. UNPACK_ALIGNMENT and UNPACK_ROW_LENGTH do not apply (WebGL 2.0 5.35).

// The TexImageSource `value` is, as the upload carries it, or null for a value that is none -- a TypeError, as WebIDL
// converts the union. A snapshot `ImageData` whose bytes were not read is its snapshot, still on the GPU; a canvas this
// runtime made is its 2D pixels (a WebGL canvas's, those of the 2D canvas showing them); a decoded image or ImageBitmap
// is the host's, at its natural size; other `ImageData`, and a canvas this runtime did not make, are their RGBA8 rows.
function texImageSourceOf(value) {
    if (value === null || typeof value !== "object" || ArrayBufferIsView(value)) return null;
    const snapshotId = value.__migo_snapshot_id__ | 0;
    if (snapshotId !== 0) {
        return { kind: TEX_SOURCE_SNAPSHOT, id: snapshotId, width: value.width | 0, height: value.height | 0, pixels: null, value };
    }
    if (_migoIsHTMLCanvas(value)) {
        return { kind: TEX_SOURCE_CANVAS, id: _migoSourceRid(value), width: value.width | 0, height: value.height | 0, pixels: null, value };
    }
    if (value.__migo_text_cache_key__) {
        // A text-cache hit's `ImageData`: its rows exist once `.data` is read, which paints the text. A whole
        // `texImage2D` of it takes the cached text instead (`_migoTexImageFromTextCache`), before this is read.
        return { kind: TEX_SOURCE_PIXELS, id: 0, width: value.width | 0, height: value.height | 0, pixels: null, value };
    }
    if (typeof value.rid === "number") {
        const width = value.naturalWidth === undefined ? value.width : value.naturalWidth;
        const height = value.naturalHeight === undefined ? value.height : value.naturalHeight;
        return { kind: TEX_SOURCE_IMAGE, id: value.rid >>> 0, width: width | 0, height: height | 0, pixels: null, value };
    }
    const raw = sourceToRawRgba(value);
    if (raw === null) return null;
    return { kind: TEX_SOURCE_PIXELS, id: 0, width: raw.width, height: raw.height, pixels: raw.data, value };
}

// `texImageSourceOf`, or the TypeError WebIDL throws for a value that is no TexImageSource.
function requireTexImageSource(value, method) {
    const source = texImageSourceOf(value);
    if (source === null) {
        throw new TypeError(`${method}: the source is not an ImageBitmap, ImageData, image, canvas or video`);
    }
    return source;
}

// An upload from `source` once the call's own checks have passed (`uploadTexImageSource`). `destination` is a sub
// call's image's internal format as the renderer's copy reads it (`_sourceCopyFormat`), 0 for a full call. A snapshot
// taken here is spent: nothing is left for the frame's end to read back. Nothing is sent for a sub call of no pixels;
// a full call of none defines an empty image, as an upload of no bytes does.
function texImageFromSourceOf(
    ctx, call, target, level, internalformat, xoffset, yoffset, zoffset, width, height, depth, format, type,
    destination, source,
) {
    const full = call === TEX_SOURCE_CALL_IMAGE_2D || call === TEX_SOURCE_CALL_IMAGE_3D;
    if (width === 0 || height === 0 || depth === 0) {
        if (!full) return;
        if (call === TEX_SOURCE_CALL_IMAGE_2D) {
            _rawTexImage2D(ctx._canvasId, target, level, internalformat, width, height, 0, format, type, null, -1);
        } else {
            _rawTexImage3D(ctx._canvasId, target, level, internalformat, width, height, depth, 0, format, type, null, -1);
        }
        return;
    }
    const pixels = source.kind === TEX_SOURCE_PIXELS && source.pixels === null
        ? toUnit8Array(source.value.data) : source.pixels;
    uploadTexImageSource(
        ctx._canvasId, call, Number(target) >>> 0, level, Number(internalformat) >>> 0, xoffset, yoffset, zoffset,
        width, height, depth, Number(format) >>> 0, Number(type) >>> 0, destination,
        source.kind, source.id, source.width, source.height, pixels,
    );
    if (source.kind === TEX_SOURCE_SNAPSHOT) source.value.__migo_snapshot_spent__ = true;
}

// A full 2D upload of a whole canvas whose 2D context holds pending text-cache state -- cocos's labels -- takes the
// cached text or records it (`_consumeTextCacheForTexImage`): true when that sent the upload. Any other upload of the
// canvas settles the state first, painting a suppressed `fillText` so the pixels it reads are the canvas's.
function settleCanvasTextCache(ctx, source, wholeImage2D, target, level, internalformat, format, type) {
    if (source.kind !== TEX_SOURCE_CANVAS) return false;
    const context = source.value._context;
    if (!context || typeof context._consumeTextCacheForTexImage !== "function") return false;
    if (wholeImage2D) {
        return context._consumeTextCacheForTexImage(ctx._canvasId, target, level, internalformat, format, type);
    }
    context._abandonPendingTextCache();
    return false;
}

// The internal format a sub upload's pixels end in, as the renderer's GPU copy judges it (`copy_reproduces`): the
// image's effective sized format, the unsized LUMINANCE / ALPHA / LUMINANCE_ALPHA of bytes (they have none), or 0 --
// no copy -- for an image this facade has not recorded.
function _sourceCopyFormat(image) {
    if (image === undefined || image.compressed) return 0;
    const effective = _effectiveFormat(image);
    if (effective !== undefined) return effective;
    const i = image.internalformat;
    return (i === 0x1909 || i === 0x1906 || i === 0x190a) && image.type === _UBYTE ? i : 0;
}

class WebGLRenderingContext {
    constructor(canvas, options) {
        this._canvas = canvas;
        this._options = options || {};
        this._canvasId = canvas._rid;
        // Which interface this is, read on every call whose rules differ (`_isWebGL2`); WebGL 2's constructor sets it.
        this._webgl2 = false;
        // Lost through WEBGL_lose_context: this context only (see getExtension).
        this._lostByExtension = false;
        // Resource IDs are allocated from a runtime-global counter in Rust.
        // Nested Map: programId -> Map(name -> location)
        // Allows O(1) per-program invalidation via .delete(programId).
        this._attribLocationCache = new Map();
        this._uniformLocationCache = new Map();
        this._programParameterCache = new Map();
        // Program introspection. Every one of these was a synchronous round
        // trip on a value that cannot change until the program is relinked, and
        // the note in contracts/runtime/synchronous-surface.json says Emscripten
        // exports walk every attribute and every uniform of every program at
        // load -- so the stalls arrive in a batch, during startup, which is the
        // budget Android has been measuring.
        // programId -> Map(index -> {size, type, name})
        this._activeAttribCache = new Map();
        this._activeUniformCache = new Map();
        // programId -> Map(name -> block index)
        this._uniformBlockIndexCache = new Map();
        // programId -> Map(index -> {size, type, name}), transform feedback
        this._transformFeedbackVaryingCache = new Map();
        // shaderId -> Map(pname -> value)
        this._shaderParameterCache = new Map();
        // Vertex attribute state, for `getVertexAttrib`: the bound vertex array object's per-attribute array state, and the
        // constant values `vertexAttrib*` set (one set for the context, as GL has it; their bits are shared between the
        // float, int and uint views, and `_currentAttribKind` says which the last call wrote).
        this._attribDefaults = new VertexAttribShadow();
        this._attribShadow = this._attribDefaults;
        const currentBits = new ArrayBuffer(_ATTRIB_SHADOW_SLOTS * 16);
        this._currentAttribF = new Float32Array(currentBits);
        this._currentAttribI = new Int32Array(currentBits);
        this._currentAttribU = new Uint32Array(currentBits);
        this._currentAttribKind = new Uint8Array(_ATTRIB_SHADOW_SLOTS);
        for (let i = 0; i < _ATTRIB_SHADOW_SLOTS; i++) this._currentAttribF[i * 4 + 3] = 1;
        this._maxVertexAttribs = 0;
        this._attribMinimum = 8;      // MAX_VERTEX_ATTRIBS is at least this (WebGL 1; WebGL 2 raises it)
        this._textureUnitMinimum = 8; // MAX_COMBINED_TEXTURE_IMAGE_UNITS is at least this (WebGL 1; WebGL 2 raises it)
        // The pixel-store state as `pixelStorei` set it: what `getParameter` answers and what an upload's length is
        // checked with. PACK_ALIGNMENT, UNPACK_ALIGNMENT, UNPACK_COLORSPACE_CONVERSION_WEBGL; WebGL 2 adds the row
        // lengths, image height and skips.
        this._pixelStore = new Map([[0x0d05, 4], [0x0cf5, 4], [0x9243, 0x9244]]);
        this._maxTextureUnits = 0;
        // Scratch for the scalar integer-vector setters (`uniform2i`..`4i`): the stream copies the words as it
        // encodes them, so one array per width serves every call without allocating.
        this._uniformI32Scratch = [null, null, new Uint32Array(2), new Uint32Array(3), new Uint32Array(4)];

        // Client-side binding state. `getParameter(<X>_BINDING)` must return the
        // bound wrapper object (or null), per the WebGL spec -- engines commonly
        // save/restore bindings via `bindX(target, gl.getParameter(X_BINDING))`,
        // which requires the wrapper, not a raw GL handle. The render thread only
        // knows native GL handles, so we track the JS-side objects here.
        this._activeTextureUnit = 0x84c0; // TEXTURE0
        this._textureBindings2D = new Map(); // texture unit -> WebglObject|null
        this._textureBindingsCube = new Map(); // texture unit -> WebglObject|null
        // WebGL 1: every binding of a texture that samples as incomplete, as `unit * 2 + (cube map ? 1 : 0)`; a draw holds
        // none there (`withholdIncompleteTextures`). Empty in WebGL 2, whose completeness the driver's is.
        this._incompleteBindings = new Set();
        this._arrayBufferBinding = null;
        this._vertexArrayBinding = null;     // the vertex array object bound, null for the default one
        this._programBinding = null;
        // Two, because WebGL 2 has two framebuffer binding points. A single slot
        // made `getParameter` answer the draw binding for a read bind and vice
        // versa, which is only invisible while READ_FRAMEBUFFER is unreachable.
        this._framebufferBinding = null;      // DRAW, and FRAMEBUFFER_BINDING
        this._readFramebufferBinding = null;  // READ_FRAMEBUFFER_BINDING
        this._renderbufferBinding = null;

        // Producer-side capability shadow; see _TOGGLEABLE_CAPS above.
        this._capBits = _CAP_INITIAL;
        this._capGeneration = _capGeneration;
        // The stencil masks as set (`stencilMasksOf`), filled on first use.
        this._stencilMasks = new Uint32Array(4);
        this._stencilGeneration = -1;

        // The drawing buffer's attributes (WebGL 1.0 5.2), each a dictionary member WebIDL converts to a boolean, or
        // the specification's default when absent: alpha, depth, premultipliedAlpha and antialias default true,
        // stencil, preserveDrawingBuffer and the rest false. The renderer gives the drawing buffer exactly the buffers
        // asked for (`GLCmd::WebglContext`, encoded below ahead of any command of this context), so a context without
        // stencil has none and its stencil test cannot fail. Antialiasing is a request an implementation may decline,
        // and this one does -- the drawing buffer is single-sampled -- so the context reports none, as
        // `getContextAttributes()` must report what the buffer has.
        const opts = this._options;
        const flag = (value, fallback) => (value === undefined ? fallback : !!value);
        const alpha = flag(opts.alpha, true);
        const depth = flag(opts.depth, true);
        const stencil = flag(opts.stencil, false);
        const preserveDrawingBuffer = flag(opts.preserveDrawingBuffer, false);
        const powerPref =
            opts.powerPreference === "high-performance"
                ? 1
                : opts.powerPreference === "low-power"
                ? 2
                : 0;
        op_webgl_record_attributes(
            this._canvasId,
            alpha,
            false,
            depth,
            stencil,
            flag(opts.premultipliedAlpha, true),
            preserveDrawingBuffer,
            powerPref,
            flag(opts.failIfMajorPerformanceCaveat, false),
            flag(opts.desynchronized, false),
            flag(opts.xrCompatible, false),
        );
        // What reads of the default framebuffer read from: the drawing buffer's colour format, and READ_BUFFER (BACK, or
        // NONE once `readBuffer` says so).
        this._drawingBufferFormat = alpha ? 0x8058 : 0x8051;      // RGBA8, RGB8
        this._defaultReadBuffer = 0x0405;                         // BACK
        this._defaultDrawBuffer = 0x0405;                         // BACK: DRAW_BUFFER0 of the default framebuffer
        // opcode 73: H C U, the frame_wire::gl::WEBGL_CONTEXT_* bits.
        encodeWebglContext(
            this._canvasId,
            (alpha ? 1 : 0) | (depth ? 2 : 0) | (stencil ? 4 : 0) | (preserveDrawingBuffer ? 8 : 0),
        );
    }

    /** Invalidate all cached locations/params for a given program. O(1). */
    _invalidateProgramCaches(programId) {
        this._attribLocationCache.delete(programId);
        this._uniformLocationCache.delete(programId);
        this._programParameterCache.delete(programId);
        // Relinking renumbers the active attributes and uniforms and can change
        // a uniform block's index, so these go with the rest. Adding a cache
        // here and forgetting this line is the whole failure mode: the values
        // stay plausible and belong to the previous link.
        this._activeAttribCache.delete(programId);
        this._activeUniformCache.delete(programId);
        this._uniformBlockIndexCache.delete(programId);
        this._transformFeedbackVaryingCache.delete(programId);
    }

    /**
     * One `{size, type, name}` record, from cache or across the boundary.
     *
     * The cached value is the raw record; a fresh object is handed out on every
     * call. Browsers return a new WebGLActiveInfo each time, and a shared object
     * would let one caller's mutation reach the next -- a cache that hands out
     * its own storage is a cache that content can corrupt.
     */
    _activeInfo(cache, raw, programId, index) {
        let inner = cache.get(programId);
        let record = inner && inner.get(index);
        if (record === undefined) {
            const json = raw(this._canvasId, programId, index);
            if (!json) return null;
            try {
                record = JSON.parse(json);
            } catch (_) {
                return null;
            }
            if (!inner) {
                inner = new Map();
                cache.set(programId, inner);
            }
            inner.set(index, record);
        }
        return { size: record.size, type: record.type, name: record.name };
    }

    get canvas() {
        return this._canvas;
    }

    set drawingBufferColorSpace(value) {
        console.log(`Setting drawingBufferColorSpace to ${value}`);
        throw new Error("drawingBufferColorSpace not supported");
    }

    get drawingBufferColorSpace() {
        throw new Error("drawingBufferColorSpace not supported");
    }

    // WebGL 1.0 section 2.2: "HTMLCanvasElement.width and .height values less than 1 are treated as 1. A 0x0
    // canvas will yield a 1x1 drawingBufferWidth/Height." The drawing buffer behind a zero-sized canvas is
    // one pixel (see `pbuffer_extent` in graphics), and this must say so.
    get drawingBufferWidth() {
        return this._canvas ? MathMax(1, this._canvas.width) : 0;
    }

    get drawingBufferHeight() {
        return this._canvas ? MathMax(1, this._canvas.height) : 0;
    }

    set unpackColorSpace(value) {
        console.log(`Setting unpackColorSpace to ${value}`);
        throw new Error("unpackColorSpace not supported");
    }

    get unpackColorSpace() {
        throw new Error("unpackColorSpace not supported");
    }

    // A negative width or height is the decoder's INVALID_VALUE, for both lanes, as for `scissor`.
    viewport(x, y, width, height) {
        // Encodability: all 4 are i32. Check typeof number before x|0.
        if (typeof x === "number" && typeof y === "number" &&
            typeof width === "number" && typeof height === "number") {
            encodeViewport(this._canvasId, x | 0, y | 0, width | 0, height | 0);
            return;
        }
        // Raw fallback: flush pending stream then call original op.
        flushRenderCommandStream();
        op_viewport(this._canvasId, x, y, width, height);
    }

    clearColor(r, g, b, a) {
        if (typeof r === "number" && typeof g === "number" &&
            typeof b === "number" && typeof a === "number") {
            encodeClearColor(this._canvasId, r, g, b, a);
            return;
        }
        _rawClearColor(this._canvasId, r, g, b, a);
    }

    // `mask` is a GLbitfield (WebIDL `unsigned long`: a BigInt or a Symbol is a TypeError). A bit naming no buffer is
    // INVALID_VALUE, then a draw framebuffer that is not complete INVALID_FRAMEBUFFER_OPERATION, and nothing is cleared
    // -- which the render side also counts on: a clear that reaches it is one that happens.
    clear(mask) {
        const bits = +mask >>> 0;
        if ((bits & ~CLEAR_BUFFER_BITS) !== 0) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
            return;
        }
        if (refusesIncompleteFramebuffer(this, this._framebufferBinding)) return;
        encodeClear(this._canvasId, bits);
    }

    // `flush()`: what is recorded goes to the render side now (a barrier flush of the frame collector), and the
    // context's commands are submitted there (a record the render side flushes the context for) -- what a fence or a
    // query waits on. `finish()` is the same: it does not wait for the GPU, which nothing in WebGL can observe but a
    // fence or a query, and those answer on their own. Cocos calls `finish()` on resume (onShow) to drain what was
    // queued before the surface was lost. A lost context's flush does nothing.
    flush() {
        if (this.isContextLost()) return;
        encodeFlush(this._canvasId);
        _rawGlFlush();
    }

    finish() {
        this.flush();
    }

    createProgram() {
        const id = nextResourceId();
        // op_create_program: ordered raw (not in encoded set).
        _rawCreateProgram(this._canvasId, id);
        return new WebglObject(id, "program", this._canvasId);
    }

    // A program that did not link, a deleted one or another context's is INVALID_OPERATION, and the program in use
    // stays in use (ES 3.0 2.12.3). Whether it linked is asked once per link and kept, as `getProgramParameter`
    // keeps it -- a browser asks its GPU process the same question the same way.
    useProgram(program) {
        const bound = program === undefined ? null : program;
        if (bound !== null) {
            if (!(bound instanceof WebglObject) || bound._kind !== "program") {
                throw new TypeError("Failed to execute 'useProgram' on 'WebGLRenderingContext': parameter 1 is not of type 'WebGLProgram'.");
            }
            if (bound._deleted || bound._ownerId !== this._canvasId ||
                    !this.getProgramParameter(bound, WebglConstants.LINK_STATUS)) {
                recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
                return;
            }
        }
        this._programBinding = bound;
        // useProgram: opcode 8, H C U. 0 uses none.
        encodeUseProgram(this._canvasId, bound ? bound._id >>> 0 : 0);
    }

    linkProgram(program) {
        const programId = program?.id;
        _rawLinkProgram(programId);
        if (programId !== undefined) {
            // Linking can change active attrib/uniform locations and link status, and a uniform location from an
            // earlier link is no longer the program's (`getUniform`).
            this._invalidateProgramCaches(programId);
            program._links = (program._links | 0) + 1;
        }
    }

    getProgramParameter(program, pname) {
        const programId = program?.id;
        if (programId === undefined) return 0;
        // The shaders attached are the facade's to know (see `attachShader`); a cached count went stale on the next
        // attach or detach.
        if (pname === 0x8b85) return program._shaders ? program._shaders.length : 0;   // ATTACHED_SHADERS
        let inner = this._programParameterCache.get(programId);
        if (inner) {
            const cached = inner.get(pname);
            if (cached !== undefined) {
                if (
                    pname === WebglConstants.DELETE_STATUS ||
                    pname === WebglConstants.VALIDATE_STATUS ||
                    pname === WebglConstants.LINK_STATUS
                ) {
                    return Boolean(cached);
                }
                return cached;
            }
        } else {
            inner = new Map();
            this._programParameterCache.set(programId, inner);
        }
        if (pname === WebglConstants.LINK_STATUS && program instanceof WebglObject && !program._deleted &&
                program._ownerId === this._canvasId) {
            return this._linkResult(program);
        }
        const param = _rawGetProgramParameter(programId, pname);
        inner.set(pname, param);
        if (
            pname === WebglConstants.DELETE_STATUS ||
            pname === WebglConstants.VALIDATE_STATUS ||
            pname === WebglConstants.LINK_STATUS
        ) {
            return Boolean(param);
        }
        return param;
    }

    // What a link made of a program, asked once per link in place of LINK_STATUS (which every engine and `useProgram`
    // ask anyway, so it costs no crossing of its own): whether it linked, and every attribute location it consumes,
    // which a draw's vertex ranges are checked against (`_attribRangeError`). The answer is cached until the next link.
    _linkResult(program) {
        const answer = this._programState("getProgramParameter", program, GL_STATE_LINK_RESULT, 0, "");
        const linked = answer !== undefined && answer[0] === true;
        let inner = this._programParameterCache.get(program._id);
        if (inner === undefined) {
            inner = new Map();
            this._programParameterCache.set(program._id, inner);
        }
        inner.set(WebglConstants.LINK_STATUS, linked ? 1 : 0);
        program._consumes = linked ? answer[1] : [];
        program._consumesLink = program._links | 0;
        return linked;
    }

    getProgramInfoLog(program) {
        return _rawGetProgramInfoLog(program?.id);
    }

    deleteProgram(program) {
        if (program instanceof WebglObject) program._deleted = true;
        const programId = program?.id;
        _rawDeleteProgram(programId);
        if (programId !== undefined) {
            this._invalidateProgramCaches(programId);
        }
    }

    createShader(type) {
        const shaderType = type >>> 0;
        if (shaderType !== 0x8b31 && shaderType !== 0x8b30) {     // VERTEX_SHADER, FRAGMENT_SHADER
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return null;
        }
        const id = nextResourceId();
        _rawCreateShader(this._canvasId, id, shaderType);
        const shader = new WebglObject(id, "shader", this._canvasId);
        shader._type = shaderType;
        return shader;
    }

    shaderSource(shader, src) {
        if (!allowWebglShaderSource(this._canvasId, src)) return;
        // What getShaderSource answers: the string the content gave, as a string.
        if (shader) shader._source = String(src);
        return _rawShaderSource(this._canvasId, shader?.id, src);
    }

    getShaderSource(shader) {
        return shader && shader._source !== undefined ? shader._source : "";
    }

    compileShader(shader) {
        _rawCompileShader(shader?.id);
    }

    getShaderParameter(shader, pname) {
        const shaderId = shader?.id;
        if (shaderId === undefined) return 0;
        // SHADER_TYPE is immutable after creation -- always cacheable.
        if (pname === WebglConstants.SHADER_TYPE) {
            let inner = this._shaderParameterCache.get(shaderId);
            if (inner) {
                const cached = inner.get(pname);
                if (cached !== undefined) return cached;
            } else {
                inner = new Map();
                this._shaderParameterCache.set(shaderId, inner);
            }
            const val = _rawGetShaderParameter(shaderId, pname);
            inner.set(pname, val);
            return val;
        }
        const ret = _rawGetShaderParameter(shaderId, pname);
        if (pname === WebglConstants.COMPILE_STATUS || pname === WebglConstants.DELETE_STATUS) {
            return Boolean(ret);
        }
        return ret;
    }

    // What is attached to a program is kept on it, and `getAttachedShaders` and ATTACHED_SHADERS answer from there.
    // Attaching a shader already attached, or one of a type already attached, is INVALID_OPERATION; so is detaching
    // one that is not. A deleted program or shader, or another context's, is INVALID_OPERATION; a value that is not
    // one is a TypeError.
    _checkProgramAndShader(name, program, shader) {
        if (!(program instanceof WebglObject) || program._kind !== "program") {
            throw new TypeError(`Failed to execute '${name}' on 'WebGLRenderingContext': parameter 1 is not of type 'WebGLProgram'.`);
        }
        if (!(shader instanceof WebglObject) || shader._kind !== "shader") {
            throw new TypeError(`Failed to execute '${name}' on 'WebGLRenderingContext': parameter 2 is not of type 'WebGLShader'.`);
        }
        if (!this._isLive(program, "program") || !this._isLive(shader, "shader")) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return false;
        }
        return true;
    }
    attachShader(program, shader) {
        if (!this._checkProgramAndShader("attachShader", program, shader)) return;
        const attached = program._shaders || (program._shaders = []);
        for (let k = 0; k < attached.length; k++) {
            if (attached[k] === shader || attached[k]._type === shader._type) {
                recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
                return;
            }
        }
        attached.push(shader);
        _rawAttachShader(program._id, shader._id);
    }
    detachShader(program, shader) {
        if (!this._checkProgramAndShader("detachShader", program, shader)) return;
        const attached = program._shaders;
        let at = -1;
        if (attached) for (let k = 0; k < attached.length; k++) if (attached[k] === shader) at = k;
        if (at < 0) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return;
        }
        attached.splice(at, 1);
        _rawDetachShader(program._id, shader._id);
    }
    getAttachedShaders(program) {
        if (!(program instanceof WebglObject) || program._kind !== "program") {
            throw new TypeError("Failed to execute 'getAttachedShaders' on 'WebGLRenderingContext': parameter 1 is not of type 'WebGLProgram'.");
        }
        if (!this._isLive(program, "program")) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return null;
        }
        return program._shaders ? program._shaders.slice() : [];
    }
    // VALIDATE_STATUS and the info log change with it, so the cached VALIDATE_STATUS goes.
    validateProgram(program) {
        if (!(program instanceof WebglObject) || program._kind !== "program") {
            throw new TypeError("Failed to execute 'validateProgram' on 'WebGLRenderingContext': parameter 1 is not of type 'WebGLProgram'.");
        }
        if (!this._isLive(program, "program")) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return;
        }
        const cached = this._programParameterCache.get(program._id);
        if (cached) cached.delete(0x8b83);     // VALIDATE_STATUS
        _rawValidateProgram(program._id);
    }
    sampleCoverage(value, invert) {
        encodeSampleCoverage(this._canvasId, +value, !!invert);
    }

    getShaderInfoLog(shader) {
        return _rawGetShaderInfoLog(shader?.id);
    }

    deleteShader(shader) {
        if (shader instanceof WebglObject) shader._deleted = true;
        const shaderId = shader?.id;
        _rawDeleteShader(shaderId);
        if (shaderId !== undefined) {
            this._shaderParameterCache.delete(shaderId);
        }
    }

    // Every draw samples no incomplete WebGL 1 texture (`withholdIncompleteTextures`).
    drawArrays(mode, first, count) {
        const error = this._drawError(mode, Number(first) | 0, Number(count) | 0, 1, undefined, 0);
        if (error !== 0) {
            recordGpuPreflightError(this._canvasId, error);
            return;
        }
        const withheld = this._incompleteBindings.size !== 0;
        if (withheld) withholdIncompleteTextures(this);
        // opcode 47: H C U I I. mode is u32, first/count are i32.
        if (typeof mode === "number" && typeof first === "number" && typeof count === "number") {
            encodeDrawArrays(this._canvasId, mode >>> 0, first | 0, count | 0);
        } else {
            flushRenderCommandStream();
            _rawDrawArrays(this._canvasId, mode, first, count);
        }
        if (withheld) restoreIncompleteTextures(this);
    }

    // What every draw is checked for before it is sent, in the order a browser checks it: a mode that is no primitive
    // is INVALID_ENUM, and so is an index type the context does not take (UNSIGNED_INT needs OES_element_index_uint in
    // WebGL 1); a negative first, count, instance count or offset INVALID_VALUE; an offset that is not a multiple of the
    // index size, no program in use (ES 3.0 2.12.3), no element-array buffer or a range of indices past its end
    // INVALID_OPERATION (WebGL 1.0 6.4, 6.6); then the vertices the draw reads (`_attribRangeError`). `indexType` is
    // undefined for a draw of arrays. 0 when none is. The decoder checks the arguments again, for the records that do
    // not come through here.
    // `instancedANGLE`: the call is WebGL 1's `draw*InstancedANGLE`, which draws only with an attribute array enabled at
    // divisor 0 (ANGLE_instanced_arrays) -- any one, as a browser counts it; WebGL 2's instanced calls have no such rule.
    _drawError(mode, first, count, instances, indexType, offset, instancedANGLE = false) {
        if ((Number(mode) >>> 0) > 6) return GL_INVALID_ENUM;              // POINTS .. TRIANGLE_FAN
        let bytes = 0;
        if (indexType !== undefined) {
            const t = Number(indexType) >>> 0;
            bytes = t === 0x1401 ? 1 : t === 0x1403 ? 2
                : t === 0x1405 && (this._webgl2 || this._oesElementIndexUint !== undefined) ? 4 : 0;
            if (bytes === 0) return GL_INVALID_ENUM;
        }
        if (first < 0 || count < 0 || instances < 0 || offset < 0) return GL_INVALID_VALUE;
        if (bytes !== 0 && offset % bytes !== 0) return GL_INVALID_OPERATION;
        if (this._programBinding === null) return GL_INVALID_OPERATION;
        if (framebufferStatus(this, this._framebufferBinding) !== GL_FRAMEBUFFER_COMPLETE) return GL_INVALID_FRAMEBUFFER_OPERATION;
        // Blending into a 32-bit float colour buffer takes EXT_float_blend: INVALID_OPERATION where the driver has none.
        // Only a float attachment EXT_color_buffer_float -- WebGL 1's WEBGL_color_buffer_float -- made complete can be one.
        const fb = this._framebufferBinding;
        if (fb !== null && (this._extColorBufferFloat !== undefined || this._webglColorBufferFloat !== undefined) &&
                this.isEnabled(0x0be2) &&
                (this._gpuCaps & 8) === 0 && drawsIntoFloat32(this, fb)) return GL_INVALID_OPERATION;
        let indices = null;
        if (bytes !== 0) {
            indices = this._attribShadow.elementArrayBuffer;
            if (indices === null || offset + count * bytes > (indices._size || 0)) return GL_INVALID_OPERATION;
        }
        if (count === 0 || instances === 0) return 0;
        if (instancedANGLE && !_hasEnabledDivisorZero(this._attribShadow)) return GL_INVALID_OPERATION;
        return this._attribRangeError(first, count, instances, indices, bytes, offset);
    }

    // Every attribute the program in use consumes that is enabled as an array must have a buffer, and the buffer must
    // hold each element the draw reads: the vertices `first` .. `first + count - 1` -- an indexed draw's largest index
    // (`largestIndex`) for the last -- or, for an instanced attribute of divisor d, the instances 0 ..
    // ceil(instances / d) - 1. Either missing is INVALID_OPERATION (WebGL 1.0 6.5, 6.6).
    _attribRangeError(first, count, instances, indices, bytes, offset) {
        const program = this._programBinding;
        if (program._consumesLink !== (program._links | 0)) this._linkResult(program);
        const consumed = program._consumes;
        const shadow = this._attribShadow;
        let lastVertex = -2;    // not yet known
        for (let k = 0; k < consumed.length; k++) {
            const location = consumed[k];
            if (location >= _ATTRIB_SHADOW_SLOTS || shadow.enabled[location] === 0) continue;
            const buffer = shadow.buffer[location];
            if (buffer === null) return GL_INVALID_OPERATION;
            const divisor = shadow.divisor[location];
            let last;
            if (divisor !== 0) {
                last = MathCeil(instances / divisor) - 1;
            } else {
                if (lastVertex === -2) {
                    lastVertex = indices === null ? first + count - 1
                        : largestIndex(indices, bytes, offset, count, this._webgl2);
                }
                last = lastVertex;
            }
            if (last < 0) continue;
            const element = shadow.size[location] * _attribComponentBytes(shadow.type[location]);
            const stride = shadow.stride[location] || element;
            if (shadow.offset[location] + last * stride + element > (buffer._size || 0)) return GL_INVALID_OPERATION;
        }
        return 0;
    }

    drawElements(mode, count, type, offset) {
        const c = Number(count) | 0, t = Number(type) >>> 0, o = toLongLong(Number(offset));
        const error = this._drawError(mode, 0, c, 1, t, o);
        if (error !== 0) {
            recordGpuPreflightError(this._canvasId, error);
            return;
        }
        const withheld = this._incompleteBindings.size !== 0;
        if (withheld) withholdIncompleteTextures(this);
        // opcode 48: H C U I U I. mode/type are u32, count/offset are i32.
        if (typeof mode === "number" && typeof count === "number" &&
            typeof type === "number" && typeof offset === "number") {
            encodeDrawElements(this._canvasId, mode >>> 0, c, t, o);
        } else {
            flushRenderCommandStream();
            _rawDrawElements(this._canvasId, mode, count, type, offset);
        }
        if (withheld) restoreIncompleteTextures(this);
    }

    // A name WebGL refuses is INVALID_VALUE, and a reserved one INVALID_OPERATION (WebGL 1.0 6.20).
    bindAttribLocation(program, index, name) {
        const programId = program?.id;
        if (programId === undefined) return;
        const key = `${name}`;
        const error = _glslNameError(key, this._maxNameLength());
        if (error !== 0 || _isReservedGlslName(key)) {
            recordGpuPreflightError(this._canvasId, error !== 0 ? error : GL_INVALID_OPERATION);
            return;
        }
        _rawBindAttribLocation(programId, index >>> 0, key);
        // Locations only change on the next link; drop any cached lookups.
        this._attribLocationCache.delete(programId);
    }

    // True for an object this context made, of that kind, and not deleted.
    _isLive(object, kind) {
        return object instanceof WebglObject && object._kind === kind &&
            object._ownerId === this._canvasId && !object._deleted;
    }
    isBuffer(object) { return this._isLive(object, "buffer") && object._everBound === true; }
    isFramebuffer(object) { return this._isLive(object, "framebuffer") && object._everBound === true; }
    isProgram(object) { return this._isLive(object, "program"); }
    isRenderbuffer(object) { return this._isLive(object, "renderbuffer") && object._everBound === true; }
    isShader(object) { return this._isLive(object, "shader"); }
    isTexture(object) { return this._isLive(object, "texture") && object._target !== undefined; }

    isContextLost() {
        // Direct, no submit: op_gl_is_context_lost is host-local.
        return this._lostByExtension || op_gl_is_context_lost();
    }

    // WEBGL_lose_context's two halves. The flag changes now and the event is
    // fired from a task, as the extension specifies. Losing a lost context and
    // restoring one the extension did not lose change nothing.
    _setLostByExtension(lost) {
        if (this._lostByExtension === lost) return;
        this._lostByExtension = lost;
        const type = lost ? "webglcontextlost" : "webglcontextrestored";
        setTimeout(() => {
            let prevented = false;
            this._canvas.dispatchEvent({
                type,
                statusMessage: "",
                bubbles: false,
                cancelable: lost,
                get defaultPrevented() { return prevented; },
                preventDefault() { if (lost) prevented = true; },
            });
        }, 0);
    }

    getShaderPrecisionFormat(_shaderType, precisionType) {
        // Migo is a WebGL2 / GLES3 context: highp is guaranteed in both vertex
        // and fragment shaders. Return spec-correct GLES3 values. Integer
        // precision types are HIGH_INT/MEDIUM_INT/LOW_INT (0x8DF3..0x8DF5).
        const isInt = precisionType >= 0x8df3 && precisionType <= 0x8df5;
        return isInt
            ? { rangeMin: 31, rangeMax: 30, precision: 0 }
            : { rangeMin: 127, rangeMax: 127, precision: 23 };
    }

    // The longest name a lookup by name takes (WebGL 1.0 6.21; WebGL 2.0 raises it to 1024).
    _maxNameLength() {
        return this._isWebGL2() ? 1024 : 256;
    }

    // A name WebGL refuses is INVALID_VALUE and -1, and a reserved one finds nothing; neither asks GL. Both are checked
    // only past the cache, which holds nothing but names GL was asked.
    getAttribLocation(program, name) {
        const programId = program?.id;
        if (programId === undefined) return -1;
        const key = `${name}`;
        let inner = this._attribLocationCache.get(programId);
        if (inner) {
            const cached = inner.get(key);
            if (cached !== undefined) return cached;
        }
        const error = _glslNameError(key, this._maxNameLength());
        if (error !== 0) {
            recordGpuPreflightError(this._canvasId, error);
            return -1;
        }
        if (_isReservedGlslName(key)) return -1;
        if (!inner) {
            inner = new Map();
            this._attribLocationCache.set(programId, inner);
        }
        const location = _rawGetAttribLocation(this._canvasId, programId, key);
        inner.set(key, location);
        return location;
    }

    getActiveAttrib(program, index) {
        const programId = program?.id;
        if (programId === undefined) return null;
        return this._activeInfo(
            this._activeAttribCache, _rawGetActiveAttrib, programId, index >>> 0,
        );
    }

    getActiveUniform(program, index) {
        const programId = program?.id;
        if (programId === undefined) return null;
        return this._activeInfo(
            this._activeUniformCache, _rawGetActiveUniform, programId, index >>> 0,
        );
    }

    enableVertexAttribArray(index) {
        // opcode 16: H C U. index is u32.
        if (typeof index === "number") {
            encodeEnableVertexAttribArray(this._canvasId, index >>> 0);
        } else {
            flushRenderCommandStream();
            _rawEnableVertexAttribArray(this._canvasId, index);
        }
        const i = Number(index) >>> 0;
        if (i < _ATTRIB_SHADOW_SLOTS) this._attribShadow.enabled[i] = 1;
    }

    vertexAttribPointer(index, size, type, normalized, stride, offset) {
        // opcode 18: H C U I U B I I.
        // index/type are u32, size/stride/offset are i32, normalized is bool.
        if (typeof index === "number" && typeof size === "number" &&
            typeof type === "number" && typeof normalized === "boolean" &&
            typeof stride === "number" && typeof offset === "number") {
            encodeVertexAttribPointer(
                this._canvasId,
                index >>> 0,
                size | 0,
                type >>> 0,
                normalized,
                stride | 0,
                offset | 0,
            );
        } else {
            flushRenderCommandStream();
            op_vertex_attrib_pointer(
                this._canvasId,
                index,
                size,
                type,
                normalized,
                stride,
                offset,
            );
        }
        this._shadowAttribPointer(index, size, type, normalized, false, stride, offset);
    }

    _shadowAttribPointer(index, size, type, normalized, integer, stride, offset) {
        const i = Number(index) >>> 0;
        size = Number(size) | 0; type = Number(type) >>> 0; stride = Number(stride) | 0; offset = Number(offset);
        if (i >= _ATTRIB_SHADOW_SLOTS || !_attribPointerAccepted(size, type, stride, offset, integer)) return;
        const sh = this._attribShadow;
        sh.size[i] = size;
        sh.type[i] = type;
        sh.normalized[i] = integer || !normalized ? 0 : 1;
        sh.integer[i] = integer ? 1 : 0;
        sh.stride[i] = stride;
        sh.offset[i] = offset;
        sh.buffer[i] = this._arrayBufferBinding;
    }

    createBuffer() {
        const id = nextResourceId();
        _rawCreateBuffer(this._canvasId, id);
        const buffer = new WebglObject(id, "buffer", this._canvasId);
        buffer._elements = null;        // an element-array buffer's bytes (`largestIndex`), once it has data
        buffer._indexRanges = null;     // offset -> (count, index size, restart) -> its largest index
        return buffer;
    }

    // ---- Buffers ------------------------------------------------------------------------------------------------
    // What each target has bound, and each buffer's size and usage, are kept here: `getParameter`,
    // `getBufferParameter` and the range checks of the calls that read or write a buffer answer from them.

    // The buffer bound to `target`: null for none, undefined for a target this context does not have (WebGL 2 adds
    // its own in its override). ELEMENT_ARRAY_BUFFER is the bound vertex array object's.
    _boundBuffer(target) {
        if (target === 0x8892) return this._arrayBufferBinding;
        if (target === 0x8893) return this._attribShadow.elementArrayBuffer;
        return undefined;
    }
    // Binds `buffer` (or null) to a target `_boundBuffer` knows.
    _setBoundBuffer(target, buffer) {
        if (target === 0x8892) this._arrayBufferBinding = buffer;
        else this._attribShadow.elementArrayBuffer = buffer;
    }
    // Whether binding `buffer` to `target` is refused: 0 for null or a live buffer of this context whose WebGL type
    // takes the target, else the error (WebGL 1.0 6.1, WebGL 2.0 5.1: a buffer is element-array or other data from its
    // first bind -- COPY_READ_BUFFER and COPY_WRITE_BUFFER take either -- so index data is never also vertex data). A
    // value that is not a buffer is a TypeError, as WebIDL converts it.
    _bufferBindError(method, argument, buffer, target) {
        if (buffer === null) return 0;
        if (!(buffer instanceof WebglObject) || buffer._kind !== "buffer") {
            throw new TypeError(`Failed to execute '${method}' on 'WebGLRenderingContext': parameter ${argument} is not of type 'WebGLBuffer'.`);
        }
        if (buffer._deleted || buffer._ownerId !== this._canvasId) return GL_INVALID_OPERATION;
        const type = buffer._webglType;
        if (type === undefined || target === 0x8f36 || target === 0x8f37) return 0;
        return type === (target === 0x8893 ? "element" : "other") ? 0 : GL_INVALID_OPERATION;
    }
    // A bind took effect: a buffer bound for the first time takes its WebGL type, and from now on is a buffer
    // (`isBuffer` is false for one never bound, as `glIsBuffer` is).
    _noteBufferBound(buffer, target) {
        if (buffer === null) return;
        if (buffer._webglType === undefined) buffer._webglType = target === 0x8893 ? "element" : "other";
        buffer._everBound = true;
    }
    // WebGL 1's three usages; WebGL 2 adds the READ and COPY ones.
    _bufferUsageTakes(usage) {
        return usage === 0x88e4 || usage === 0x88e8 || usage === 0x88e0 ||       // STATIC_DRAW, DYNAMIC_DRAW, STREAM_DRAW
            (this._isWebGL2() && (usage === 0x88e1 || usage === 0x88e2 || usage === 0x88e5 ||
                                  usage === 0x88e6 || usage === 0x88e9 || usage === 0x88ea));
    }
    // A deleted buffer leaves every binding of it in this context and in the container objects bound to it -- the
    // vertex array object's element buffer and attributes; WebGL 2 adds its targets and the transform feedback
    // object's (ES 3.0 2.10.1, D.1.2). An object not bound keeps its reference.
    _unbindDeletedBuffer(buffer) {
        if (this._arrayBufferBinding === buffer) this._arrayBufferBinding = null;
        const attribs = this._attribShadow;
        if (attribs.elementArrayBuffer === buffer) attribs.elementArrayBuffer = null;
        for (let i = 0; i < _ATTRIB_SHADOW_SLOTS; i++) if (attribs.buffer[i] === buffer) attribs.buffer[i] = null;
    }

    deleteBuffer(buffer) {
        if (!this._deletes("deleteBuffer", buffer, "buffer", "WebGLBuffer")) return;
        buffer._deleted = true;
        this._unbindDeletedBuffer(buffer);
        _rawDeleteBuffer(buffer._id);
    }

    bindBuffer(target, buffer) {
        const t = Number(target) >>> 0;
        if (this._boundBuffer(t) === undefined) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return;
        }
        const bound = buffer === undefined ? null : buffer;
        const error = this._bufferBindError("bindBuffer", 2, bound, t);
        if (error !== 0) {
            recordGpuPreflightError(this._canvasId, error);
            return;
        }
        this._noteBufferBound(bound, t);
        this._setBoundBuffer(t, bound);
        const bufferId = bound ? bound._id : -1;
        // opcode 9: H C U I. bufferId is i32 (negative = unbind).
        if (typeof target === "number") {
            encodeBindBuffer(this._canvasId, t, bufferId);
            return;
        }
        flushRenderCommandStream();
        _rawBindBuffer(this._canvasId, target, bufferId);
    }

    // `bufferData(target, size, usage)` and `bufferData(target, data, usage)`, and WebGL 2's
    // `bufferData(target, view, usage, srcOffset, length)`, whose range is counted in the view's elements. A target
    // the context does not have and a usage it does not take are INVALID_ENUM, no buffer bound INVALID_OPERATION, a
    // negative size and null data INVALID_VALUE. The buffer's size and usage are what the call gave it.
    bufferData(target, srcOrSize, usage, srcOffset, length) {
        const t = Number(target) >>> 0;
        const bound = this._boundBuffer(t);
        const u = Number(usage) >>> 0;
        if (bound === undefined || !this._bufferUsageTakes(u)) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return;
        }
        if (bound === null) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return;
        }
        if (typeof srcOrSize === "number" || typeof srcOrSize === "bigint") {
            const size = toLongLong(Number(srcOrSize));
            if (size < 0) {
                recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
                return;
            }
            if (!allowWebglUpload(this._canvasId, size)) return;
            bound._size = size;
            bound._usage = u;
            if (bound._webglType === "element") {
                bound._elements = new Uint8Array(size);
                elementBytesChanged(bound);
            }
            return _rawBufferData(this._canvasId, t, size, null, u);
        }
        if (srcOrSize === null || srcOrSize === undefined) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
            return;
        }
        const u8 = arguments.length > 3 && this._isWebGL2()
            ? this._viewArgumentBytes("bufferData", srcOrSize, srcOffset, length)
            : toBoundedUploadBytes(this._canvasId, srcOrSize);
        if (u8 === null) return;
        bound._size = TypedArrayPrototypeGetByteLength(u8);
        bound._usage = u;
        if (bound._webglType === "element") {
            bound._elements = new Uint8Array(u8);
            elementBytesChanged(bound);
        }
        return _rawBufferData(this._canvasId, t, -1, u8, u);
    }

    // WebGL 2's overloads that take `srcOffset` (and `length`; WebGL 1 has none, and WebIDL ignores the extra
    // arguments): `data` must be an ArrayBufferView, a TypeError otherwise as WebIDL's overload resolution has it, and
    // the range is `viewElementBytes`'s.
    _viewArgumentBytes(method, data, srcOffset, length) {
        if (!ArrayBufferIsView(data)) {
            throw new TypeError(`Failed to execute '${method}' on 'WebGL2RenderingContext': the source is not an ArrayBufferView.`);
        }
        return viewElementBytes(this._canvasId, data, srcOffset, length);
    }

    getBufferParameter(target, pname) {
        const bound = this._boundBuffer(Number(target) >>> 0);
        if (bound === undefined) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return null;
        }
        if (bound === null) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return null;
        }
        const p = Number(pname) >>> 0;
        if (p === 0x8764) return bound._size || 0;                         // BUFFER_SIZE
        if (p === 0x8765) return bound._usage || 0x88e4;                   // BUFFER_USAGE, STATIC_DRAW
        recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
        return null;
    }

    // A location belongs to the program, under the name it was asked by, until the program links again: `getUniform`
    // asks the driver by that name. Names are checked as `getAttribLocation` checks them (null for one refused or reserved).
    getUniformLocation(program, name) {
        const programId = program?.id;
        if (programId === undefined) return null;
        const key = `${name}`;
        let inner = this._uniformLocationCache.get(programId);
        if (inner) {
            const cached = inner.get(key);
            if (cached !== undefined) return cached;
        }
        const error = _glslNameError(key, this._maxNameLength());
        if (error !== 0) {
            recordGpuPreflightError(this._canvasId, error);
            return null;
        }
        if (_isReservedGlslName(key)) return null;
        if (!inner) {
            inner = new Map();
            this._uniformLocationCache.set(programId, inner);
        }
        const id = _rawGetUniformLocation(this._canvasId, programId, key);
        if (id < 0) {
            inner.set(key, null);
            return null;
        }
        const location = new WebglObject(id, "uniformLocation", this._canvasId);
        location._program = program;
        location._name = key;
        location._link = program._links | 0;
        inner.set(key, location);
        return location;
    }

    // The driver's value of the uniform at `location`: what `uniform*` set since the program last linked, or 0. A
    // location another program gave, or this one before it linked again, is INVALID_OPERATION and null, as are a
    // deleted program and one that did not link.
    getUniform(program, location) {
        if (!(program instanceof WebglObject) || program._kind !== "program") {
            throw new TypeError("Failed to execute 'getUniform' on 'WebGLRenderingContext': parameter 1 is not of type 'WebGLProgram'.");
        }
        if (!(location instanceof WebglObject) || location._kind !== "uniformLocation") {
            throw new TypeError("Failed to execute 'getUniform' on 'WebGLRenderingContext': parameter 2 is not of type 'WebGLUniformLocation'.");
        }
        if (location._program !== program || location._link !== (program._links | 0)) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return null;
        }
        const answer = this._programState("getUniform", program, GL_STATE_UNIFORM_VALUE, 0, location._name);
        return answer === undefined ? null : _uniformValue(answer[0], answer[1]);
    }
    // What a linked program says about itself that only the driver knows (`program_state.rs` answers): its uniform
    // blocks -- PlayCanvas reads every block's name while it links a shader; engines with a uniform-buffer layer read
    // the sizes and the offsets -- a uniform's value and a fragment output's location. The answer is `{v}` or `{e}`:
    // the error is the specification's, raised here so the context's `getError` sees it.
    _programState(method, program, query, extra, name) {
        if (!(program instanceof WebglObject) || program._kind !== "program") {
            throw new TypeError(`${method}: parameter 1 is not of type 'WebGLProgram'.`);
        }
        if (program._deleted || program._ownerId !== this._canvasId) {
            recordGpuPreflightError(this._canvasId, WebglConstants.INVALID_OPERATION);
            return undefined;
        }
        let answer;
        try { answer = JSON.parse(_rawGetGlState(this._canvasId, query, program._id, extra, name)); } catch (_) { return undefined; }
        if (answer === null || typeof answer !== "object") return undefined;
        if (answer.e !== undefined) {
            recordGpuPreflightError(this._canvasId, answer.e);
            return undefined;
        }
        return answer.v;
    }


    uniform3f(location, x, y, z) {
        // opcode 57: H C I F F F. location is i32, x/y/z are f32.
        // All f32 values are encodable. Check location is a number (or null -> -1).
        const loc = _loc(location);
        encodeUniform3f(this._canvasId, loc, +x, +y, +z);
    }

    uniformMatrix3fv(location, transpose, value) {
        const loc = _loc(location);
        const payload = toFloat32AsUint32(value);
        if (typeof transpose !== "boolean" ||
            !encodeUniformMatrix3fv(this._canvasId, loc, transpose, payload)) {
            // Payload > 512 words: flush pending stream, then call raw op.
            flushRenderCommandStream();
            op_uniform_matrix_3fv(this._canvasId, loc, transpose, payload);
        }
    }

    // -- Phase 1A: GL State --

    // Refill the capability shadow if the GL context has been rebuilt since it
    // was last written. One integer compare on the enable/disable path.
    _freshCapBits() {
        if (this._capGeneration !== _capGeneration) {
            this._capBits = _CAP_INITIAL;
            this._capGeneration = _capGeneration;
        }
    }

    // The capability's bit, or undefined for an enum that is not one of this context's (WebGL 1 has no
    // RASTERIZER_DISCARD).
    _capBitOf(cap) {
        const bit = _CAP_BIT.get(cap);
        return bit === _RASTERIZER_DISCARD_BIT && !this._isWebGL2() ? undefined : bit;
    }

    // An enum that is not a capability is INVALID_ENUM and changes nothing; nothing is sent, as the driver's error
    // would not reach `getError`.
    enable(cap) {
        const bit = this._capBitOf(Number(cap) >>> 0);
        if (bit === undefined) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return;
        }
        this._freshCapBits();
        this._capBits |= bit;
        // opcode 6: H C U.
        if (typeof cap === "number") {
            encodeEnable(this._canvasId, cap >>> 0);
            return;
        }
        flushRenderCommandStream();
        _rawEnable(this._canvasId, cap);
    }

    disable(cap) {
        const bit = this._capBitOf(Number(cap) >>> 0);
        if (bit === undefined) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return;
        }
        this._freshCapBits();
        this._capBits &= ~bit;
        // opcode 7: H C U.
        if (typeof cap === "number") {
            encodeDisable(this._canvasId, cap >>> 0);
            return;
        }
        flushRenderCommandStream();
        _rawDisable(this._canvasId, cap);
    }

    isEnabled(cap) {
        const bit = this._capBitOf(Number(cap) >>> 0);
        if (bit === undefined) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return false;
        }
        this._freshCapBits();
        return (this._capBits & bit) !== 0;
    }

    getParameter(pname) {
        pname = Number(pname) >>> 0;
        // Binding-state queries return the JS-side wrapper object (or null), per
        // the WebGL spec, so `bindX(target, getParameter(X_BINDING))` round-trips.
        switch (pname) {
            case 0x8069: return this._textureBindings2D.get(this._activeTextureUnit) || null; // TEXTURE_BINDING_2D
            case 0x8514: return this._textureBindingsCube.get(this._activeTextureUnit) || null; // TEXTURE_BINDING_CUBE_MAP
            // TEXTURE_BINDING_3D, TEXTURE_BINDING_2D_ARRAY: WebGL 2's.
            case 0x806a: case 0x8c1d: {
                const bindings = this._textureBindings(pname === 0x806a ? 0x806f : 0x8c1a);
                if (bindings !== undefined) return bindings.get(this._activeTextureUnit) || null;
                recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
                return null;
            }
            case 0x8894: return this._arrayBufferBinding; // ARRAY_BUFFER_BINDING
            case 0x8895: return this._attribShadow.elementArrayBuffer; // ELEMENT_ARRAY_BUFFER_BINDING
            // WebGL 2's buffer bindings, by the target each names (COPY_READ_BUFFER_BINDING is COPY_READ_BUFFER, and
            // so for COPY_WRITE); a WebGL 1 context has none of them.
            case 0x8f36: case 0x8f37:
            case 0x88ed: case 0x88ef: case 0x8a28: case 0x8c8f: {
                const target = pname === 0x88ed ? 0x88eb : pname === 0x88ef ? 0x88ec    // PIXEL_PACK, PIXEL_UNPACK
                    : pname === 0x8a28 ? 0x8a11 : pname === 0x8c8f ? 0x8c8e : pname;    // UNIFORM, TRANSFORM_FEEDBACK
                const bound = this._boundBuffer(target);
                if (bound !== undefined) return bound;
                recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
                return null;
            }
            // READ_BUFFER (WebGL 2): the read framebuffer's, as `readBuffer` recorded it.
            case 0x0c02: {
                if (!this._isWebGL2()) {
                    recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
                    return null;
                }
                const fb = this._readFramebufferBinding;
                return fb === null ? this._defaultReadBuffer : fb._readBuffer === undefined ? 0x8ce0 : fb._readBuffer;
            }
            // DRAW_BUFFER0..15 (WebGL 2, WEBGL_draw_buffers): the draw framebuffer's, as `drawBuffers` recorded it --
            // BACK then NONE for the default framebuffer, COLOR_ATTACHMENT0 then NONE for an object until it is set.
            // Past MAX_DRAW_BUFFERS, or without either, INVALID_ENUM.
            case 0x8825: case 0x8826: case 0x8827: case 0x8828: case 0x8829: case 0x882a: case 0x882b: case 0x882c:
            case 0x882d: case 0x882e: case 0x882f: case 0x8830: case 0x8831: case 0x8832: case 0x8833: case 0x8834: {
                const index = pname - 0x8825;
                if ((!this._isWebGL2() && this._webglDrawBuffers === undefined) ||
                        (index >= 4 && index >= this._drawBufferLimit())) {
                    recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
                    return null;
                }
                const fb = this._framebufferBinding;
                if (fb === null) return index === 0 ? this._defaultDrawBuffer : 0;
                if (fb._drawBuffers === undefined) return index === 0 ? 0x8ce0 : 0;
                return index < fb._drawBuffers.length ? fb._drawBuffers[index] : 0;
            }
            // SAMPLER_BINDING (WebGL 2): the sampler bound to the active texture unit.
            case 0x8919:
                if (this._isWebGL2()) return this._samplerBindings.get(this._activeTextureUnit - 0x84c0) || null;
                recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
                return null;
            // TRANSFORM_FEEDBACK_BINDING (WebGL 2): the object bound, null for the default one.
            case 0x8e25:
                if (this._isWebGL2()) return this._currentTransformFeedback;
                recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
                return null;
            // VERTEX_ARRAY_BINDING (WebGL 2), VERTEX_ARRAY_BINDING_OES (WebGL 1, once the extension is enabled).
            case 0x85b5:
                if (this._oesVertexArrayObject || this._isWebGL2()) return this._vertexArrayBinding;
                recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
                return null;
            case 0x8b8d: return this._programBinding; // CURRENT_PROGRAM
            // FRAMEBUFFER_BINDING and DRAW_FRAMEBUFFER_BINDING are one enum
            // (0x8CA6) in GLES 3, so this arm answers both.
            case 0x8ca6: return this._framebufferBinding;
            case 0x8caa: return this._readFramebufferBinding; // READ_FRAMEBUFFER_BINDING
            case 0x8ca7: return this._renderbufferBinding; // RENDERBUFFER_BINDING
            // MAX_CLIENT_WAIT_TIMEOUT_WEBGL. Answered here because it is a
            // WebGL-only limit this context chooses, not something the driver
            // knows -- asking it would have crossed for a constant. Zero: see
            // `clientWaitSync`.
            case 0x9247:
                if (this._isWebGL2()) return 0;
                recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
                return null;
            // The stencil masks: GLuint, as set (`stencilMasksOf`).
            case 0x0b93: return stencilMasksOf(this)[0];       // STENCIL_VALUE_MASK
            case 0x8ca4: return stencilMasksOf(this)[1];       // STENCIL_BACK_VALUE_MASK
            case 0x0b98: return stencilMasksOf(this)[2];       // STENCIL_WRITEMASK
            case 0x8ca5: return stencilMasksOf(this)[3];       // STENCIL_BACK_WRITEMASK
            // COMPRESSED_TEXTURE_FORMATS: the formats of the compressed-texture extensions enabled, as a browser lists
            // them.
            case 0x86a3: {
                const formats = [];
                for (const format of _COMPRESSED_FORMATS.keys()) {
                    if (this._compressedFormat(format) !== undefined) formats.push(format);
                }
                return new Uint32Array(formats);
            }
            // The two flags are booleans in WebGL.
            case 0x9240: return this._unpackFlipY === true;
            case 0x9241: return this._unpackPremultiplyAlpha === true;
            // The rest of the pixel-store state, as `pixelStorei` set it. The WebGL 2 pnames are not WebGL 1's.
            case 0x0d05: case 0x0cf5: case 0x9243:      // PACK_ALIGNMENT, UNPACK_ALIGNMENT, UNPACK_COLORSPACE_CONVERSION_WEBGL
            case 0x0d02: case 0x0d04: case 0x0d03:      // PACK_ROW_LENGTH, PACK_SKIP_PIXELS, PACK_SKIP_ROWS
            case 0x0cf2: case 0x806e: case 0x0cf4: case 0x0cf3: case 0x806d: {   // UNPACK_ROW_LENGTH, _IMAGE_HEIGHT, the skips
                const value = this._pixelStore.get(pname);
                if (value !== undefined) return value;
                recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
                return null;
            }
            // `VERSION` and `SHADING_LANGUAGE_VERSION` begin with the WebGL version, then the driver's own
            // string in parentheses. Content (and libraries) tell WebGL 1 from 2 by this prefix.
            case 0x1f02: return this._webglVersionString(0x1f02, "WebGL");
            // VENDOR and RENDERER are a browser's masked ones; the driver's are WEBGL_debug_renderer_info's
            // UNMASKED_VENDOR_WEBGL and UNMASKED_RENDERER_WEBGL, answered whether or not it is enabled, as browsers now
            // answer them, and asked of the driver once.
            case 0x1f00: return "WebKit";
            case 0x1f01: return "WebKit WebGL";
            case 0x9245: return this._driverString("_unmaskedVendor", 0x1f00);
            case 0x9246: return this._driverString("_unmaskedRenderer", 0x1f01);
            // MAX_TEXTURE_MAX_ANISOTROPY_EXT: EXT_texture_filter_anisotropic's, asked of the driver once.
            case 0x84ff:
                if (this._extTextureFilterAnisotropic === undefined) {
                    recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
                    return null;
                }
                return this._maxAnisotropy();
            case 0x8b8c: return this._webglVersionString(0x8b8c, "WebGL GLSL ES");
            // FRAGMENT_SHADER_DERIVATIVE_HINT: WebGL 2's, OES_standard_derivatives' in WebGL 1.
            case 0x8b8b:
                if (this._isWebGL2() || this._oesStandardDerivatives !== undefined) {
                    return _typedParameter(_rawGetParameter(this._canvasId, pname), _P_NUMBER);
                }
                recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
                return null;
            // IMPLEMENTATION_COLOR_READ_FORMAT / _TYPE: the driver's pair for the read buffer, which must be one a read
            // can be made from (INVALID_OPERATION otherwise, as `_refusesRead` judges it); WebGL 1 names a 16-bit float
            // HALF_FLOAT_OES.
            case 0x8b9a: case 0x8b9b: {
                if (framebufferStatus(this, this._readFramebufferBinding) !== GL_FRAMEBUFFER_COMPLETE ||
                        readColorFormat(this) === 0) {
                    recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
                    return null;
                }
                let value = null;
                try { value = JSON.parse(_rawGetParameter(this._canvasId, pname)); } catch (_) { /* none */ }
                return value === _HALF && !this._isWebGL2() ? 0x8d61 : value;
            }
            default: break;
        }
        // A capability queried through getParameter is the same GLboolean
        // isEnabled returns -- the spec defines them as one answer. Once
        // isEnabled stopped crossing, leaving this arm to ask the driver made
        // the two able to disagree: the driver's bit is shared with Skia and
        // with the engine's own scissor use, so `isEnabled(BLEND)` and
        // `getParameter(BLEND)` could return different booleans for the same
        // context. They go through one shadow.
        if (this._capBitOf(pname) !== undefined) {
            return this.isEnabled(pname);
        }
        // The driver's, typed as WebGL has it (`_DRIVER_PARAMETERS`); MAX_COLOR_ATTACHMENTS and MAX_DRAW_BUFFERS are
        // WEBGL_draw_buffers' in WebGL 1.
        const spec = _DRIVER_PARAMETERS.get(pname);
        if (spec === undefined || ((spec[0] & (this._isWebGL2() ? _WEBGL2_ONLY : _WEBGL1_ONLY)) === 0 &&
                !((pname === 0x8cdf || pname === 0x8824) && this._webglDrawBuffers !== undefined))) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return null;
        }
        return _typedParameter(_rawGetParameter(this._canvasId, pname), spec[1]);
    }

    // The driver's string for `pname`, asked once and kept in `field`.
    _driverString(field, pname) {
        if (this[field] === undefined) {
            let value = "";
            try { value = JSON.parse(_rawGetParameter(this._canvasId, pname)); } catch (_) { /* none: an empty one */ }
            this[field] = typeof value === "string" ? value : "";
        }
        return this[field];
    }

    // MAX_TEXTURE_MAX_ANISOTROPY_EXT, asked of the driver once: at least 1 (a driver that offers the extension has 2 or
    // more).
    _maxAnisotropy() {
        if (this._maxAnisotropyCache === undefined) {
            let value = 1;
            try { value = JSON.parse(_rawGetParameter(this._canvasId, 0x84ff)); } catch (_) { /* none */ }
            this._maxAnisotropyCache = typeof value === "number" && value >= 1 ? value : 1;
        }
        return this._maxAnisotropyCache;
    }

    // `<prefix> <version> (<driver string>)`, with the version of the interface this object is.
    _webglVersionString(pname, prefix) {
        const raw = _rawGetParameter(this._canvasId, pname);
        let driver = "";
        try { driver = JSON.parse(raw); } catch (_) { /* no string: an empty one */ }
        const two = typeof WebGL2RenderingContext === "function" && this instanceof WebGL2RenderingContext;
        const version = pname === 0x1f02 ? (two ? "2.0" : "1.0") : (two ? "3.00" : "1.00");
        return `${prefix} ${version} (${driver})`;
    }

    // One queue per context, of flags -- a code once until it is read (`WebGLErrorState::push`) -- holding what the
    // facade refused and what the decoder refused. The stream is sent first, so a refusal the decoder makes of a
    // record already written is in the queue before it is read.
    getError() {
        flushRenderCommandStream();
        return op_webgl_get_error(this._canvasId);
    }

    getContextAttributes() {
        // Direct, no-submit: op_webgl_get_context_attributes is host-local.
        return op_webgl_get_context_attributes(this._canvasId);
    }

    // The extension `name` names, compared case-insensitively as WebGL compares them, when this context offers it
    // (`_EXTENSIONS`): made once and the same object on every call, which enables it. Null for one this context does
    // not offer, and for any on a lost context.
    getExtension(name) {
        if (this.isContextLost()) return null;
        // WebIDL's DOMString: a Symbol is a TypeError.
        const key = StringPrototypeToLowerCase(`${name}`);
        // Migo's own, for verification: a simulated GPU reset -- the whole share group lost and rebuilt, with
        // webglcontextlost/restored on the main canvas -- which no device can be made to do on demand. Not listed: no
        // content asks for it by accident.
        if (key === "migo_debug_gpu_reset") {
            return this._migoDebugGpuReset ||
                (this._migoDebugGpuReset = {
                    reset: () => { _rawGlLoseContext(this._canvasId); },
                });
        }
        const extension = _EXTENSIONS_BY_KEY.get(key);
        if (extension === undefined || !offersExtension(this, extension)) return null;
        const [, , , field, build] = extension;
        if (this[field] === undefined) {
            this._enabling = field;
            this[field] = build(this);
        }
        return this[field];
    }

    // The names of the extensions this context offers (`_EXTENSIONS`), or null for a lost context.
    getSupportedExtensions() {
        if (this.isContextLost()) return null;
        const list = [];
        for (const extension of _EXTENSIONS) {
            if (offersExtension(this, extension)) list.push(extension[0]);
        }
        return list;
    }

    // The renderer's capabilities, read once per context: the render thread publishes them before any JS GL call
    // completes (`op_webgl_query_gpu_caps`, `GpuCaps::webgl_bits`): bit 0 ETC2/EAC, 1 ASTC, 2 float colour buffers,
    // 3 blending into 32-bit floats, 4 anisotropic filtering, 5 16-bit float colour buffers, 6 filtering 32-bit floats.
    get _gpuCaps() {
        if (this._gpuCapsCache === undefined) {
            this._gpuCapsCache = op_webgl_query_gpu_caps() | 0;
        }
        return this._gpuCapsCache;
    }

    _buildWebglDrawBuffers() {
        // Enum table from the WEBGL_draw_buffers extension spec.
        // The numeric values match the GLES 3.0 core enums
        // (`GL_COLOR_ATTACHMENT0_WEBGL == GL_COLOR_ATTACHMENT0`),
        // so we can forward the untransformed buffer list straight
        // to `op_draw_buffers`.
        const ctx = this;
        const obj = {
            COLOR_ATTACHMENT0_WEBGL: 0x8CE0,
            COLOR_ATTACHMENT1_WEBGL: 0x8CE1,
            COLOR_ATTACHMENT2_WEBGL: 0x8CE2,
            COLOR_ATTACHMENT3_WEBGL: 0x8CE3,
            COLOR_ATTACHMENT4_WEBGL: 0x8CE4,
            COLOR_ATTACHMENT5_WEBGL: 0x8CE5,
            COLOR_ATTACHMENT6_WEBGL: 0x8CE6,
            COLOR_ATTACHMENT7_WEBGL: 0x8CE7,
            COLOR_ATTACHMENT8_WEBGL: 0x8CE8,
            COLOR_ATTACHMENT9_WEBGL: 0x8CE9,
            COLOR_ATTACHMENT10_WEBGL: 0x8CEA,
            COLOR_ATTACHMENT11_WEBGL: 0x8CEB,
            COLOR_ATTACHMENT12_WEBGL: 0x8CEC,
            COLOR_ATTACHMENT13_WEBGL: 0x8CED,
            COLOR_ATTACHMENT14_WEBGL: 0x8CEE,
            COLOR_ATTACHMENT15_WEBGL: 0x8CEF,
            DRAW_BUFFER0_WEBGL: 0x8825,
            DRAW_BUFFER1_WEBGL: 0x8826,
            DRAW_BUFFER2_WEBGL: 0x8827,
            DRAW_BUFFER3_WEBGL: 0x8828,
            DRAW_BUFFER4_WEBGL: 0x8829,
            DRAW_BUFFER5_WEBGL: 0x882A,
            DRAW_BUFFER6_WEBGL: 0x882B,
            DRAW_BUFFER7_WEBGL: 0x882C,
            DRAW_BUFFER8_WEBGL: 0x882D,
            DRAW_BUFFER9_WEBGL: 0x882E,
            DRAW_BUFFER10_WEBGL: 0x882F,
            DRAW_BUFFER11_WEBGL: 0x8830,
            DRAW_BUFFER12_WEBGL: 0x8831,
            DRAW_BUFFER13_WEBGL: 0x8832,
            DRAW_BUFFER14_WEBGL: 0x8833,
            DRAW_BUFFER15_WEBGL: 0x8834,
            MAX_COLOR_ATTACHMENTS_WEBGL: 0x8CDF,
            MAX_DRAW_BUFFERS_WEBGL: 0x8824,
            drawBuffersWEBGL(buffers) {
                drawBuffersOf(ctx, buffers);
            },
        };
        return obj;
    }

    _buildCompressedEtc() {
        // ETC2/EAC format enum block.  No methods - data upload
        // goes through `compressedTexImage2D` like every other
        // compressed extension.  Values mirror the GLES 3.0 core
        // internal-format constants so our existing
        // `op_compressed_tex_image_2d` accepts them unchanged.
        return {
            COMPRESSED_R11_EAC: 0x9270,
            COMPRESSED_SIGNED_R11_EAC: 0x9271,
            COMPRESSED_RG11_EAC: 0x9272,
            COMPRESSED_SIGNED_RG11_EAC: 0x9273,
            COMPRESSED_RGB8_ETC2: 0x9274,
            COMPRESSED_SRGB8_ETC2: 0x9275,
            COMPRESSED_RGB8_PUNCHTHROUGH_ALPHA1_ETC2: 0x9276,
            COMPRESSED_SRGB8_PUNCHTHROUGH_ALPHA1_ETC2: 0x9277,
            COMPRESSED_RGBA8_ETC2_EAC: 0x9278,
            COMPRESSED_SRGB8_ALPHA8_ETC2_EAC: 0x9279,
        };
    }

    // WEBGL_compressed_texture_astc: the LDR profile's 28 formats, the fourteen block sizes each linear and sRGB, which
    // the uploads take once the extension is enabled (`_COMPRESSED_FORMATS`).
    _buildCompressedAstc() {
        const extension = { getSupportedProfiles: () => ["ldr"] };
        const blocks = ["4x4", "5x4", "5x5", "6x5", "6x6", "8x5", "8x6", "8x8", "10x5", "10x6", "10x8", "10x10", "12x10",
            "12x12"];
        blocks.forEach((block, k) => {
            extension[`COMPRESSED_RGBA_ASTC_${block}_KHR`] = 0x93b0 + k;
            extension[`COMPRESSED_SRGB8_ALPHA8_ASTC_${block}_KHR`] = 0x93d0 + k;
        });
        return extension;
    }

    _buildAngleInstancedArrays() {
        const ctx = this;
        return {
            // Published enum from the ANGLE_instanced_arrays spec.
            VERTEX_ATTRIB_ARRAY_DIVISOR_ANGLE: 0x88FE,
            drawArraysInstancedANGLE(mode, first, count, primcount) {
                const error = ctx._drawError(mode, Number(first) | 0, Number(count) | 0, Number(primcount) | 0, undefined, 0, true);
                if (error !== 0) {
                    recordGpuPreflightError(ctx._canvasId, error);
                    return;
                }
                const withheld = ctx._incompleteBindings.size !== 0;
                if (withheld) withholdIncompleteTextures(ctx);
                // Encode if all params are numbers; otherwise flush+raw.
                if (typeof mode === "number" && typeof first === "number" &&
                    typeof count === "number" && typeof primcount === "number") {
                    encodeDrawArraysInstanced(
                        ctx._canvasId, mode >>> 0, first | 0, count | 0, primcount | 0,
                    );
                } else {
                    flushRenderCommandStream();
                    op_draw_arrays_instanced(
                        ctx._canvasId, mode, first, count, primcount,
                    );
                }
                if (withheld) restoreIncompleteTextures(ctx);
            },
            drawElementsInstancedANGLE(mode, count, type, offset, primcount) {
                const error = ctx._drawError(mode, 0, Number(count) | 0, Number(primcount) | 0, type, toLongLong(Number(offset)), true);
                if (error !== 0) {
                    recordGpuPreflightError(ctx._canvasId, error);
                    return;
                }
                const withheld = ctx._incompleteBindings.size !== 0;
                if (withheld) withholdIncompleteTextures(ctx);
                if (typeof mode === "number" && typeof count === "number" &&
                    typeof type === "number" && typeof offset === "number" &&
                    typeof primcount === "number") {
                    encodeDrawElementsInstanced(
                        ctx._canvasId, mode >>> 0, count | 0, type >>> 0, offset | 0, primcount | 0,
                    );
                } else {
                    flushRenderCommandStream();
                    op_draw_elements_instanced(
                        ctx._canvasId, mode, count, type, offset, primcount,
                    );
                }
                if (withheld) restoreIncompleteTextures(ctx);
            },
            vertexAttribDivisorANGLE(index, divisor) {
                if (!ctx._isAttribIndex(Number(index) >>> 0)) {
                    recordGpuPreflightError(ctx._canvasId, GL_INVALID_VALUE);
                    return;
                }
                if (typeof index === "number" && typeof divisor === "number") {
                    encodeVertexAttribDivisor(ctx._canvasId, index >>> 0, divisor >>> 0);
                } else {
                    flushRenderCommandStream();
                    op_vertex_attrib_divisor(ctx._canvasId, index, divisor);
                }
                const i = Number(index) >>> 0;
                if (i < _ATTRIB_SHADOW_SLOTS) ctx._attribShadow.divisor[i] = Number(divisor) >>> 0;
            },
        };
    }

    _buildOesVertexArrayObject() {
        const ctx = this;
        return {
            VERTEX_ARRAY_BINDING_OES: 0x85B5,
            createVertexArrayOES() { return ctx._createVertexArray(); },
            deleteVertexArrayOES(vao) { ctx._deleteVertexArray("deleteVertexArrayOES", vao); },
            isVertexArrayOES(vao) { return ctx._isVertexArray(vao); },
            bindVertexArrayOES(vao) { ctx._bindVertexArray("bindVertexArrayOES", vao); },
        };
    }

    // -- Phase 1B: Textures --

    createTexture() {
        const id = nextResourceId();
        _rawCreateTexture(this._canvasId, id);
        const texture = new WebglObject(id, "texture", this._canvasId);
        texture._images = null;         // image key (`_imageKey`) -> TextureImage, once one is defined
        texture._immutableLevels = 0;   // the levels `texStorage*` fixed; 0 while the texture is mutable
        texture._params = null;         // pname -> value, as `texParameter*` set them
        return texture;
    }

    // ---- Texture bindings -----------------------------------------------------------------------------------------
    // What each unit has bound to each texture target is kept here: `getParameter` answers the objects, and every call
    // on a target's texture needs one bound. A texture takes the target it is first bound to and keeps it (ES 3.0
    // 3.8.1): binding it to another is INVALID_OPERATION.

    // The bindings of a texture target this context has (unit -> texture), or undefined. WebGL 2 adds TEXTURE_3D and
    // TEXTURE_2D_ARRAY.
    _textureBindings(target) {
        if (target === 0x0de1) return this._textureBindings2D;
        if (target === 0x8513) return this._textureBindingsCube;
        return undefined;
    }

    // The texture a call works on: undefined -- INVALID_ENUM recorded -- for a target the call does not take, null --
    // INVALID_OPERATION recorded -- when none is bound to it (ES 3.0 3.8). `kind` is the targets the call takes:
    // "image2D" an image of a 2D texture or of a cube map's face (texImage2D and its kin), "image3D" one of a 3D or
    // 2D-array texture, "storage2D" a 2D texture or a cube map (texStorage2D), "object" any texture target the
    // context has (texParameter, generateMipmap, getTexParameter).
    _textureFor(target, kind) {
        const t = Number(target) >>> 0;
        let bindingTarget = t;
        if (kind === "image2D") bindingTarget = t === 0x0de1 ? t : t >= 0x8515 && t <= 0x851a ? 0x8513 : 0;
        else if (kind === "image3D") bindingTarget = t === 0x806f || t === 0x8c1a ? t : 0;
        else if (kind === "storage2D") bindingTarget = t === 0x0de1 || t === 0x8513 ? t : 0;
        const bindings = bindingTarget === 0 ? undefined : this._textureBindings(bindingTarget);
        if (bindings === undefined) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return undefined;
        }
        const texture = bindings.get(this._activeTextureUnit) || null;
        if (texture === null) recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
        return texture;
    }

    // ---- Texture images (see `TextureImage`) ----------------------------------------------------------------------
    // The image of `texture` at a 2D image, face or 3D target and a level, or undefined.
    _image(texture, target, level) {
        return texture._images === null ? undefined : texture._images.get(_imageKey(Number(target) >>> 0, level | 0));
    }

    // An immutable texture's images are fixed (ES 3.0 3.8.4): defining one again is INVALID_OPERATION. True when refused,
    // with the error recorded.
    _refusesImmutable(texture) {
        if (texture._immutableLevels === 0) return false;
        recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
        return true;
    }

    // WebGL 1 has mipmaps of power-of-two sizes only: a level above 0 of another size is INVALID_VALUE (ES 2.0 3.7.1).
    // True when refused, with the error recorded.
    _refusesNpotLevel(level, width, height) {
        if (this._isWebGL2() || level === 0 || (_isPowerOfTwo(width) && _isPowerOfTwo(height))) return false;
        recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
        return true;
    }

    // An upload into an existing image (ES 3.0 3.8.5): one that is not there, or compressed, or that data in (format,
    // type) cannot be uploaded into (`_uploadsInto`) is INVALID_OPERATION; a region past its edge INVALID_VALUE. The
    // offsets and size are the caller's to have checked. True when refused, with the error recorded.
    _refusesSubImage(texture, target, level, format, type, x, y, z, width, height, depth) {
        const image = this._image(texture, target, level);
        const error = image === undefined || image.compressed
                || !this._uploadsInto(image, Number(format) >>> 0, Number(type) >>> 0) ? GL_INVALID_OPERATION
            : x + width > image.width || y + height > image.height || z + depth > image.depth ? GL_INVALID_VALUE : 0;
        if (error === 0) return false;
        recordGpuPreflightError(this._canvasId, error);
        return true;
    }

    // Whether data in (format, type) may be uploaded into `image`: in WebGL 2 when ES 3.0 table 3.2 or 3.3 has the
    // combination with its internal format; in WebGL 1 when they are the format and type it was defined with.
    _uploadsInto(image, format, type) {
        if (!this._isWebGL2()) return format === image.format && type === image.type;
        const i = image.internalformat;
        const unsized = _UNSIZED_UPLOAD_TYPES.get(i);
        if (unsized !== undefined) return format === i && _listHas(unsized, type);
        const sized = _SIZED_UPLOADS.get(i);
        return sized !== undefined && sized[0] === format && _listHas(sized[1], type);
    }

    // A copy from the read framebuffer into an existing image (ES 3.0 3.8.5): a level out of range or a negative offset
    // or size is INVALID_VALUE; an image that is not there, or compressed, INVALID_OPERATION; a rectangle past its edge,
    // or a layer past its last, INVALID_VALUE. True when refused, with the error recorded.
    _refusesCopyIntoImage(texture, target, level, x, y, z, width, height) {
        let error = 0;
        if (level < 0 || level >= this._levelLimit(target) || x < 0 || y < 0 || z < 0 || width < 0 || height < 0) {
            error = GL_INVALID_VALUE;
        } else {
            const image = this._image(texture, target, level);
            error = image === undefined || image.compressed ? GL_INVALID_OPERATION
                : x + width > image.width || y + height > image.height || z >= image.depth ? GL_INVALID_VALUE : 0;
        }
        if (error === 0) return false;
        recordGpuPreflightError(this._canvasId, error);
        return true;
    }

    // How many levels a texture of `target` can have: those of the largest size the context takes for it.
    _levelLimit(target) {
        return (Number(target) >>> 0) === 0x806f ? maxMipLevels(MAX_WEBGL_GPU_3D_DIMENSION) : MAX_WEBGL_GPU_2D_LEVELS;
    }

    // A compressed format's block (`_COMPRESSED_FORMATS`), or undefined when no extension enabled has the format.
    _compressedFormat(format) {
        const block = _COMPRESSED_FORMATS.get(Number(format) >>> 0);
        if (block === undefined) return undefined;
        const extension = block[3] === "etc1" ? this._webglCompressedEtc1
            : block[3] === "etc" ? this._webglCompressedEtc : this._webglCompressedAstc;
        return extension === undefined ? undefined : block;
    }

    // A compressed image's own rules, before it is defined (WEBGL_compressed_texture_etc / _astc, ES 3.0 3.8.6): a
    // format no enabled extension has is INVALID_ENUM; a level, size or border out of range INVALID_VALUE, as is a level
    // above 0 of a size that is not a power of two in WebGL 1; a 3D texture, which none of the formats can be,
    // INVALID_OPERATION; data of another length than the format's blocks over the size INVALID_VALUE; immutable storage
    // INVALID_OPERATION. True when the image may be defined; false with the error recorded.
    _acceptsCompressedImage(texture, target, level, internalformat, width, height, depth, border, byteLength) {
        const t = Number(target) >>> 0;
        const block = this._compressedFormat(internalformat);
        let error = 0;
        if (block === undefined) {
            error = GL_INVALID_ENUM;
        } else if (block[3] === "etc1" && (t === 0x806f || t === 0x8c1a)) {
            error = GL_INVALID_OPERATION;
        } else if (!NumberIsInteger(level) || level < 0 || level >= this._levelLimit(t)) {
            error = GL_INVALID_VALUE;
        } else {
            const maxAtLevel = ((t === 0x806f ? MAX_WEBGL_GPU_3D_DIMENSION : MAX_WEBGL_GPU_2D_DIMENSION) >>> level) || 1;
            const maxDepth = t === 0x806f ? maxAtLevel : t === 0x8c1a ? MAX_WEBGL_GPU_ARRAY_LAYERS : 1;
            if (!NumberIsInteger(width) || width < 0 || width > maxAtLevel ||
                    !NumberIsInteger(height) || height < 0 || height > maxAtLevel ||
                    !NumberIsInteger(depth) || depth < 0 || depth > maxDepth ||
                    (t >= 0x8515 && t <= 0x851a && width !== height) || border !== 0 ||
                    (!this._isWebGL2() && level > 0 && !(_isPowerOfTwo(width) && _isPowerOfTwo(height)))) {
                error = GL_INVALID_VALUE;
            } else if (t === 0x806f) {
                error = GL_INVALID_OPERATION;
            } else if (byteLength !== _compressedImageBytes(block, width, height, depth)) {
                error = GL_INVALID_VALUE;
            } else if (texture._immutableLevels !== 0) {
                error = GL_INVALID_OPERATION;
            }
        }
        if (error === 0) return true;
        recordGpuPreflightError(this._canvasId, error);
        return false;
    }

    // Compressed blocks into an existing image (ES 3.0 3.8.6): a format no enabled extension has is INVALID_ENUM; a
    // level out of range, a negative offset or size, or data of another length than the blocks of the region,
    // INVALID_VALUE; an image that is not there or not of that format INVALID_OPERATION; a region past its edge
    // INVALID_VALUE; one that does not start on a block, or ends inside one short of the image's edge,
    // INVALID_OPERATION. True when the upload may be sent; false with the error recorded.
    _acceptsCompressedSubImage(texture, target, level, x, y, z, width, height, depth, format, byteLength) {
        const block = this._compressedFormat(format);
        let error = 0;
        if (block === undefined) {
            error = GL_INVALID_ENUM;
        } else if (block[3] === "etc1") {
            // WEBGL_compressed_texture_etc1: an ETC1 image is defined whole or not at all.
            error = GL_INVALID_OPERATION;
        } else if (!NumberIsInteger(level) || level < 0 || level >= this._levelLimit(target) ||
                !NumberIsInteger(x) || x < 0 || !NumberIsInteger(y) || y < 0 || !NumberIsInteger(z) || z < 0 ||
                !NumberIsInteger(width) || width < 0 || !NumberIsInteger(height) || height < 0 ||
                !NumberIsInteger(depth) || depth < 0 || byteLength !== _compressedImageBytes(block, width, height, depth)) {
            error = GL_INVALID_VALUE;
        } else {
            const image = this._image(texture, target, level);
            if (image === undefined || image.internalformat !== (Number(format) >>> 0)) {
                error = GL_INVALID_OPERATION;
            } else if (x + width > image.width || y + height > image.height || z + depth > image.depth) {
                error = GL_INVALID_VALUE;
            } else if (x % block[0] !== 0 || y % block[1] !== 0 ||
                    (width % block[0] !== 0 && x + width !== image.width) ||
                    (height % block[1] !== 0 && y + height !== image.height)) {
                error = GL_INVALID_OPERATION;
            }
        }
        if (error === 0) return true;
        recordGpuPreflightError(this._canvasId, error);
        return false;
    }

    // The error `texStorage*`'s internal format is: 0 for a sized one (ES 3.0 tables 3.13 and 3.14; DEPTH_STENCIL and
    // the other unsized ones are not) or a compressed one an enabled extension has, INVALID_ENUM for anything else. A
    // compressed 3D texture is INVALID_OPERATION: no format here can be one.
    _storageFormatError(internalformat, target) {
        const i = Number(internalformat) >>> 0;
        if (_SIZED_UPLOADS.has(i)) return 0;
        const block = this._compressedFormat(i);
        if (block === undefined) return GL_INVALID_ENUM;
        const t = Number(target) >>> 0;
        return t === 0x806f || (block[3] === "etc1" && t === 0x8c1a) ? GL_INVALID_OPERATION : 0;
    }

    // The levels `generateMipmap` and sampling start and stop at: 0 and 1000 in WebGL 1; TEXTURE_BASE_LEVEL and
    // TEXTURE_MAX_LEVEL in WebGL 2, inside the levels of immutable storage (ES 3.0 3.8.10).
    _baseLevel(texture) {
        const set = this._isWebGL2() && texture._params !== null ? texture._params.get(0x813c) : undefined;
        const base = set === undefined ? 0 : set;
        return texture._immutableLevels === 0 ? base : MathMin(base, texture._immutableLevels - 1);
    }

    _maxLevel(texture) {
        const set = this._isWebGL2() && texture._params !== null ? texture._params.get(0x813d) : undefined;
        const max = set === undefined ? 1000 : set;
        return texture._immutableLevels === 0 ? max : MathMin(max, texture._immutableLevels - 1);
    }

    // The error an upload's internal format, format and type are (WebGL 1.0 5.14.8; ES 3.0 3.8.3, tables 3.2 and 3.3),
    // in the order a browser judges them: an internal format this context does not have is INVALID_VALUE, a format or
    // type it does not have INVALID_ENUM, a combination the tables do not have INVALID_OPERATION; 0 for one they do.
    // WebGL 1 has the unsized formats only, each uploaded from itself.
    _uploadFormatError(internalformat, format, type) {
        const i = Number(internalformat) >>> 0;
        const webgl2 = this._isWebGL2();
        if (!webgl2) {
            const f = Number(format) >>> 0, t = Number(type) >>> 0;
            if (!this._webgl1Format(i)) return GL_INVALID_VALUE;
            if (!this._webgl1Format(f) || !this._webgl1Type(t)) return GL_INVALID_ENUM;
            return i === f && this._webgl1Takes(f, t) ? 0 : GL_INVALID_OPERATION;
        }
        const unsized = _UNSIZED_UPLOAD_TYPES.get(i);
        const sized = webgl2 ? _SIZED_UPLOADS.get(i) : undefined;
        if (unsized === undefined && sized === undefined) return GL_INVALID_VALUE;
        const error = this._formatAndTypeError(format, type);
        if (error !== 0) return error;
        const f = Number(format) >>> 0, t = Number(type) >>> 0;
        if (unsized !== undefined) return i === f && _listHas(unsized, t) ? 0 : GL_INVALID_OPERATION;
        return sized[0] === f && _listHas(sized[1], t) ? 0 : GL_INVALID_OPERATION;
    }

    // INVALID_ENUM for a format or a type this context has none of, else 0. HALF_FLOAT_OES is WebGL 1's.
    _formatAndTypeError(format, type) {
        const f = Number(format) >>> 0, t = Number(type) >>> 0;
        if (this._isWebGL2()) {
            return isKnownUnsizedFormat(f) && isKnownPixelType(t) && t !== 0x8d61 ? 0 : GL_INVALID_ENUM;
        }
        return this._webgl1Format(f) && this._webgl1Type(t) ? 0 : GL_INVALID_ENUM;
    }

    // WebGL 1's formats -- each its own internal format -- with the extensions enabled: the unsized five, and
    // DEPTH_COMPONENT and DEPTH_STENCIL with WEBGL_depth_texture, SRGB_EXT and SRGB_ALPHA_EXT with EXT_sRGB.
    _webgl1Format(format) {
        if (_UNSIZED_UPLOAD_TYPES.has(format)) return true;
        if (format === 0x1902 || format === 0x84f9) return this._webglDepthTexture !== undefined;
        if (format === 0x8c40 || format === 0x8c42) return this._extSrgb !== undefined;
        return false;
    }

    // WebGL 1's types with the extensions enabled: the core four, FLOAT with OES_texture_float, HALF_FLOAT_OES with
    // OES_texture_half_float, UNSIGNED_SHORT, UNSIGNED_INT and UNSIGNED_INT_24_8_WEBGL with WEBGL_depth_texture.
    _webgl1Type(type) {
        if (_listHas(_WEBGL1_UPLOAD_TYPES, type)) return true;
        if (type === _FLOAT) return this._oesTextureFloat !== undefined;
        if (type === 0x8d61) return this._oesTextureHalfFloat !== undefined;
        if (type === _USHORT || type === _UINT || type === 0x84fa) return this._webglDepthTexture !== undefined;
        return false;
    }

    // Whether WebGL 1 uploads `format` as `type`: a colour format its core types, and FLOAT and HALF_FLOAT_OES with their
    // extensions; DEPTH_COMPONENT UNSIGNED_SHORT or UNSIGNED_INT, DEPTH_STENCIL UNSIGNED_INT_24_8_WEBGL; the sRGB
    // formats UNSIGNED_BYTE.
    _webgl1Takes(format, type) {
        const core = _UNSIZED_UPLOAD_TYPES.get(format);
        if (core !== undefined) {
            return _listHas(core, type) || (type === _FLOAT && this._oesTextureFloat !== undefined) ||
                (type === 0x8d61 && this._oesTextureHalfFloat !== undefined);
        }
        if (format === 0x1902) return type === _USHORT || type === _UINT;
        if (format === 0x84f9) return type === 0x84fa;
        if (format === 0x8c40 || format === 0x8c42) return type === _UBYTE;
        return false;
    }

    // WEBGL_depth_texture's depth image: of TEXTURE_2D, at level 0, without data -- INVALID_OPERATION otherwise, as a
    // browser has it. True when refused, with the error recorded.
    _refusesDepthImage(target, level, format, pixels) {
        const f = Number(format) >>> 0;
        if (this._isWebGL2() || (f !== 0x1902 && f !== 0x84f9)) return false;
        if ((Number(target) >>> 0) === 0x0de1 && level === 0 && pixels == null) return false;
        recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
        return true;
    }

    // As `_uploadFormatError`, for a TexImageSource: WebGL 2 takes the internal formats, formats and types of its table
    // (3.7.6, `_SOURCE_UPLOADS`) and no other -- an internal format outside it INVALID_VALUE, a format or a type
    // outside it INVALID_ENUM, a combination it does not list INVALID_OPERATION, judged in that order as a browser does.
    // WebGL 1's tables are all a TexImageSource's.
    _sourceUploadFormatError(internalformat, format, type) {
        if (!this._isWebGL2()) return this._uploadFormatError(internalformat, format, type);
        const i = Number(internalformat) >>> 0, f = Number(format) >>> 0, t = Number(type) >>> 0;
        const unsized = _UNSIZED_UPLOAD_TYPES.get(i);
        const sized = _SOURCE_UPLOADS.get(i);
        if (unsized === undefined && sized === undefined) return GL_INVALID_VALUE;
        if (!_listHas(_SOURCE_FORMATS, f) || !_listHas(_SOURCE_TYPES, t)) return GL_INVALID_ENUM;
        if (unsized !== undefined) return i === f && _listHas(unsized, t) ? 0 : GL_INVALID_OPERATION;
        return sized[0] === f && _listHas(sized[1], t) ? 0 : GL_INVALID_OPERATION;
    }

    // As `_sourceUploadFormatError` for an upload into an image already there: the (format, type) pair must be one the
    // TexImageSource table has.
    _sourceSubUploadFormatError(format, type) {
        if (!this._isWebGL2()) return this._subUploadFormatError(format, type);
        const f = Number(format) >>> 0, t = Number(type) >>> 0;
        if (!_listHas(_SOURCE_FORMATS, f) || !_listHas(_SOURCE_TYPES, t)) return GL_INVALID_ENUM;
        return _SOURCE_PAIRS.has(f * 0x10000 + t) ? 0 : GL_INVALID_OPERATION;
    }

    // WebGL 2's selection of a TexImageSource's pixels (5.35): from UNPACK_SKIP_PIXELS / UNPACK_SKIP_ROWS of its top
    // left, the call's size -- the source's own for the forms that take none --, and in 3D `depth` slices
    // UNPACK_IMAGE_HEIGHT rows apart (or `height`) from slice UNPACK_SKIP_IMAGES. A selection the source does not
    // hold is INVALID_OPERATION. A WebGL 1 context has no skips. True when refused, with the error recorded.
    _refusesSourceSelection(source, width, height, depth, threeD) {
        const store = this._pixelStore;
        const w = Number(width) | 0, h = Number(height) | 0, d = Number(depth) | 0;
        const skipPixels = store.get(0x0cf4) | 0, skipRows = store.get(0x0cf3) | 0;
        const stride = threeD ? ((store.get(0x806e) | 0) || h) : h;
        const skipImages = threeD ? store.get(0x806d) | 0 : 0;
        const lastSlice = skipImages + (d > 0 ? d - 1 : 0);
        if (skipPixels + w <= source.width && skipRows + lastSlice * stride + h <= source.height) return false;
        recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
        return true;
    }

    // Whether a TexImageSource upload selects from the source's top left: no skip set (WebGL 1 has none).
    _selectsFromSourceOrigin() {
        return (this._pixelStore.get(0x0cf4) | 0) === 0 && (this._pixelStore.get(0x0cf3) | 0) === 0;
    }

    // As `_uploadFormatError` for an upload into an image already there, whose internal format is the image's: the
    // (format, type) pair must be one the tables have.
    _subUploadFormatError(format, type) {
        const error = this._formatAndTypeError(format, type);
        if (error !== 0) return error;
        const f = Number(format) >>> 0, t = Number(type) >>> 0;
        if (this._isWebGL2()) return _WEBGL2_SUB_UPLOADS.has(f * 0x10000 + t) ? 0 : GL_INVALID_OPERATION;
        // A WEBGL_depth_texture image takes no sub-image upload.
        if (f === 0x1902 || f === 0x84f9) return GL_INVALID_OPERATION;
        return this._webgl1Takes(f, t) ? 0 : GL_INVALID_OPERATION;
    }

    // WebGL 2's PIXEL_UNPACK_BUFFER: an upload is from it (the offset overloads) or from the call's own data, never
    // both, so with a buffer bound every other overload is INVALID_OPERATION (WebGL 2.0 3.7.6). True when refused,
    // with the error recorded.
    _refusesUnpackBufferBound() {
        if (!this._boundBuffer(0x88ec)) return false;
        recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
        return true;
    }

    // The WebGL 2 pixel-store constraints (WebGL 2.0 5.35): an upload's rows must lie inside the data store, so
    // UNPACK_SKIP_PIXELS + width past the row length -- UNPACK_ROW_LENGTH, or the width when that is 0 -- is
    // INVALID_OPERATION, and in 3D so is UNPACK_SKIP_ROWS + height past UNPACK_IMAGE_HEIGHT or the height. WebGL 1 has
    // none of these, so never. True when refused, with the error recorded.
    _refusesUnpackRegion(width, height, threeD) {
        const store = this._pixelStore;
        const w = Number(width) | 0, h = Number(height) | 0;
        if ((store.get(0x0cf4) | 0) + w > ((store.get(0x0cf2) | 0) || w)
                || (threeD && (store.get(0x0cf3) | 0) + h > ((store.get(0x806e) | 0) || h))) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return true;
        }
        return false;
    }

    // The bytes an upload reads from `view` -- from its `srcOffset` element on, WebGL 2's overload -- or null with the
    // error recorded (WebGL 1.0 5.14.8, WebGL 2.0 3.7.6). INVALID_OPERATION is a view of another type than `type`
    // reads (FLOAT_32_UNSIGNED_INT_24_8_REV reads none), an offset past the view, or fewer bytes from it than the
    // pixel-store state lays the upload over. The bytes are exactly those the upload reads: the rest of the view is
    // not copied. A view past the one-upload ceiling is the lanes' to refuse.
    _uploadViewBytes(view, srcOffset, width, height, depth, format, type, threeD) {
        const t = Number(type) >>> 0;
        const kind = _UPLOAD_VIEWS.get(t);
        const tag = TypedArrayPrototypeGetSymbolToStringTag(view);
        if (kind === undefined || (tag !== kind[0] && !(t === _UBYTE && tag === "Uint8ClampedArray"))) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return null;
        }
        const unit = kind[1];
        const length = TypedArrayPrototypeGetLength(view);
        const first = toUnsignedLongLong(srcOffset);
        const needed = _uploadBytes(this._pixelStore, Number(width) | 0, Number(height) | 0, Number(depth) | 0,
            _uploadBytesPerPixel(Number(format) >>> 0, t), threeD);
        if (first > length || (length - first) * unit < needed) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return null;
        }
        const buffer = TypedArrayPrototypeGetBuffer(view);
        const bytes = new Uint8Array(buffer, TypedArrayPrototypeGetByteOffset(view) + first * unit, needed);
        if (!isSharedArrayBuffer(buffer)) return bytes;
        // The op borrows the bytes until it has made its own copy, and another agent could write shared memory
        // meanwhile, so it is copied here -- unless it is past the one-upload ceiling, which both lanes refuse
        // (OUT_OF_MEMORY) without copying anything.
        return allowWebglUpload(this._canvasId, needed) ? new Uint8Array(bytes) : null;
    }

    // The source of a WebGL 2 compressed upload, as the op's last three arguments (bytes, PIXEL_UNPACK_BUFFER offset,
    // its size), or null when refused with the error recorded. An ArrayBufferView is the first overload: its elements
    // from `srcOffset`, `srcLengthOverride` of them unless that is 0 (both counted in the view's elements; a range past
    // the end is INVALID_VALUE), refused with a PIXEL_UNPACK_BUFFER bound. Anything else is the second: `imageSize`
    // bytes of the bound buffer from `offset`, refused with none bound or past its end (INVALID_OPERATION).
    _compressedUploadSource(dataOrSize, srcOffsetOrOffset, srcLengthOverride) {
        if (ArrayBufferIsView(dataOrSize)) {
            if (this._refusesUnpackBufferBound()) return null;
            const bytes = viewElementBytes(this._canvasId, dataOrSize, srcOffsetOrOffset, srcLengthOverride);
            return bytes === null ? null : [bytes, -1, 0];
        }
        const buffer = this._boundBuffer(0x88ec);
        if (!buffer) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return null;
        }
        const offset = unpackBufferOffset(this._canvasId, srcOffsetOrOffset);
        if (offset < 0) return null;
        // A negative `imageSize` is the decoder's to refuse (INVALID_VALUE), for both lanes.
        const size = dataOrSize | 0;
        if (size > 0 && offset + size > (buffer._size || 0)) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return null;
        }
        return [EMPTY_UPLOAD_BYTES, offset, size];
    }

    // An upload from WebGL 2's offset overloads needs a PIXEL_UNPACK_BUFFER bound, and is refused while
    // UNPACK_FLIP_Y_WEBGL or UNPACK_PREMULTIPLY_ALPHA_WEBGL is set: those apply to pixels the facade hands over, and a
    // buffer's never pass through it (WebGL 2.0 3.7.6, 5.35). Both INVALID_OPERATION, judged before the call's other
    // arguments, as a browser judges them. True when refused, with the error recorded.
    _refusesUnpackBufferSource() {
        if (this._boundBuffer(0x88ec) && !this._unpackFlipY && !this._unpackPremultiplyAlpha) return false;
        recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
        return true;
    }

    // The offset into the bound PIXEL_UNPACK_BUFFER an upload of WebGL 2's offset overloads reads from, once
    // `_refusesUnpackBufferSource` and the call's own checks have passed; -1 with the error recorded when refused: a
    // negative offset is INVALID_VALUE; one past 2^31 - 1, an unpack region outside the data store, an offset that is
    // not a multiple of the type's size, or a range past the buffer INVALID_OPERATION (ES 3.0 3.7.1, WebGL 2.0 5.35).
    _unpackBufferOffset(offset, width, height, depth, format, type, threeD) {
        const n = unpackBufferOffset(this._canvasId, offset);
        if (n < 0 || this._refusesUnpackRegion(width, height, threeD)) return -1;
        const t = Number(type) >>> 0;
        const kind = _UPLOAD_VIEWS.get(t);
        const unit = kind !== undefined ? kind[1] : 8;      // FLOAT_32_UNSIGNED_INT_24_8_REV: two words a pixel
        const needed = _uploadBytes(this._pixelStore, Number(width) | 0, Number(height) | 0, Number(depth) | 0,
            _uploadBytesPerPixel(Number(format) >>> 0, t), threeD);
        if (n % unit !== 0 || n + needed > (this._boundBuffer(0x88ec)._size || 0)) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return -1;
        }
        return n;
    }

    // A 3D upload from a view is refused while UNPACK_FLIP_Y_WEBGL or UNPACK_PREMULTIPLY_ALPHA_WEBGL is set
    // (INVALID_OPERATION, WebGL 2.0 5.35): the flags are defined for 2D images only. True when refused, with the error
    // recorded.
    _refusesUnpackFlagsIn3D() {
        if (!this._unpackFlipY && !this._unpackPremultiplyAlpha) return false;
        recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
        return true;
    }

    // A deleted texture leaves every unit of this context and the framebuffers bound to it; a framebuffer not bound
    // keeps its attachment (ES 3.0 3.8.1, D.1.2).
    deleteTexture(texture) {
        if (!this._deletes("deleteTexture", texture, "texture", "WebGLTexture")) return;
        texture._deleted = true;
        if (texture._target !== undefined) {
            const bindings = this._textureBindings(texture._target);
            const kind = _SAMPLED_TARGETS.indexOf(texture._target);
            for (const [unit, bound] of bindings) {
                if (bound !== texture) continue;
                bindings.delete(unit);
                this._incompleteBindings.delete(unit * 4 + kind);
            }
        }
        this._detachFromBoundFramebuffers(texture);
        _rawDeleteTexture(texture._id);
    }

    bindTexture(target, texture) {
        const t = Number(target) >>> 0;
        const bindings = this._textureBindings(t);
        if (bindings === undefined) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return;
        }
        const bound = texture === undefined ? null : texture;
        if (bound !== null) {
            if (!(bound instanceof WebglObject) || bound._kind !== "texture") {
                throw new TypeError("Failed to execute 'bindTexture' on 'WebGLRenderingContext': parameter 2 is not of type 'WebGLTexture'.");
            }
            if (bound._deleted || bound._ownerId !== this._canvasId ||
                    (bound._target !== undefined && bound._target !== t)) {
                recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
                return;
            }
            bound._target = t;
        }
        bindings.set(this._activeTextureUnit, bound);
        refreshBindingSampling(this, this._activeTextureUnit, _SAMPLED_TARGETS.indexOf(t));
        const texId = bound ? bound._id : -1;
        // opcode 10: H C U I. target is u32, texId is i32 (negative = unbind).
        if (typeof target === "number") {
            encodeBindTexture(this._canvasId, t, texId);
            return;
        }
        flushRenderCommandStream();
        op_bind_texture(this._canvasId, target, texId);
    }

    // A unit past MAX_COMBINED_TEXTURE_IMAGE_UNITS is INVALID_ENUM and changes nothing. One below the minimum every
    // implementation has asks nothing.
    activeTexture(unit) {
        const index = (Number(unit) >>> 0) - 0x84c0;   // TEXTURE0
        if (index < 0 || (index >= this._textureUnitMinimum &&
                index >= this._cachedLimit("_maxTextureUnits", 0x8b4d, this._textureUnitMinimum))) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return;
        }
        this._activeTextureUnit = index + 0x84c0;
        // opcode 11: H C U.
        if (typeof unit === "number") {
            encodeActiveTexture(this._canvasId, unit >>> 0);
            return;
        }
        flushRenderCommandStream();
        op_active_texture(this._canvasId, unit);
    }

    // 9 arguments: (target, level, internalformat, width, height, border, format, type, pixels), WebGL 2's also with a
    // `srcOffset`, with a TexImageSource for the pixels, and with an offset into the bound PIXEL_UNPACK_BUFFER; 6:
    // (target, level, internalformat, format, type, source), the size the source's. Refused before anything is sent: a
    // target with no texture (`_textureFor`), a PIXEL_UNPACK_BUFFER bound -- or, for an offset, none bound or an unpack
    // flag set (`_refusesUnpackBufferSource`) --, a level out of range, formats and a type the tables do not have
    // together (`_uploadFormatError`; a TexImageSource's own table, `_sourceUploadFormatError`), a size or border out of
    // range (`preflightTexImage2D`), a WEBGL_depth_texture image that is not TEXTURE_2D's level 0 without data
    // (`_refusesDepthImage`), a WebGL 1 mipmap of a size that is not a power of two (`_refusesNpotLevel`),
    // immutable storage (`_refusesImmutable`), an unpack region outside the data store or a selection outside the
    // source (`_refusesSourceSelection`), and pixels the upload cannot read (`_uploadViewBytes`, `_unpackBufferOffset`).
    // The image is recorded once the upload is sent. A value that is none of the overloads' is a TypeError, as WebIDL
    // converts it, before anything else is judged.
    texImage2D(target, level, internalformat, a4, a5, a6, a7, a8, a9, a10) {
        if (a7 !== undefined) {
            const webgl2 = this._isWebGL2();
            const view = a9 != null && typeof a9 === "object" && ArrayBufferIsView(a9);
            if (!webgl2 && a9 != null && !view) throw new TypeError("texImage2D: pixels is not an ArrayBufferView");
            const fromBuffer = webgl2 && a9 != null && typeof a9 !== "object";
            const source = webgl2 && a9 != null && typeof a9 === "object" && !view
                ? requireTexImageSource(a9, "texImage2D") : null;
            const texture = this._textureFor(target, "image2D");
            if (!texture) return;
            if (fromBuffer ? this._refusesUnpackBufferSource() : this._refusesUnpackBufferBound()) return;
            const formatError = source !== null
                ? this._sourceUploadFormatError(internalformat, a7, a8)
                : this._uploadFormatError(internalformat, a7, a8);
            if (!preflightTexImage2D(this._canvasId, target, level, a4, a5, a6, formatError) ||
                this._refusesDepthImage(target, level, a7, a9) || this._refusesNpotLevel(level, a4, a5) ||
                this._refusesImmutable(texture)) return;
            if (source !== null) {
                if (this._refusesSourceSelection(source, a4, a5, 1, false)) return;
                const whole = a4 === source.width && a5 === source.height && this._selectsFromSourceOrigin();
                if (!(whole && _migoTexImageFromTextCache(this._canvasId, target, level, internalformat, a7, a8, a9)) &&
                    !settleCanvasTextCache(this, source, whole, target, level, internalformat, a7, a8)) {
                    texImageFromSourceOf(
                        this, TEX_SOURCE_CALL_IMAGE_2D, target, level, internalformat, 0, 0, 0, a4, a5, 1, a7, a8, 0, source,
                    );
                }
            } else if (!texImageFromData(this, target, level, internalformat, a4, a5, a6, a7, a8, a9, a10, fromBuffer)) {
                return;
            }
            defineTextureImage(texture, target, level,
                new TextureImage(Number(internalformat) >>> 0, Number(a7) >>> 0, Number(a8) >>> 0, a4, a5, 1, false));
            refreshTextureSampling(this, texture);
            return;
        }
        const source = requireTexImageSource(a6, "texImage2D");
        const texture = this._textureFor(target, "image2D");
        if (!texture) return;
        if (this._refusesUnpackBufferBound()) return;
        const width = source.width, height = source.height;
        if (!preflightTexImage2D(
            this._canvasId, target, level, width, height, 0, this._sourceUploadFormatError(internalformat, a4, a5),
        ) || this._refusesDepthImage(target, level, a4, source) || this._refusesNpotLevel(level, width, height) ||
            this._refusesImmutable(texture)) return;
        if (this._refusesSourceSelection(source, width, height, 1, false)) return;
        const whole = this._selectsFromSourceOrigin();
        if (!(whole && _migoTexImageFromTextCache(this._canvasId, target, level, internalformat, a4, a5, a6)) &&
            !settleCanvasTextCache(this, source, whole, target, level, internalformat, a4, a5)) {
            texImageFromSourceOf(
                this, TEX_SOURCE_CALL_IMAGE_2D, target, level, internalformat, 0, 0, 0, width, height, 1, a4, a5, 0, source,
            );
        }
        defineTextureImage(texture, target, level,
            new TextureImage(Number(internalformat) >>> 0, Number(a4) >>> 0, Number(a5) >>> 0, width, height, 1, false));
        refreshTextureSampling(this, texture);
    }

    // 9 arguments: (target, level, xoffset, yoffset, width, height, format, type, pixels), WebGL 2's also with a
    // `srcOffset`, with a TexImageSource for the pixels, and with an offset into the bound PIXEL_UNPACK_BUFFER; 7:
    // (target, level, xoffset, yoffset, format, type, source), the size the source's. Refused as `texImage2D` is,
    // against the (format, type) pairs of either table (`_subUploadFormatError`, `_sourceSubUploadFormatError`), then
    // against the image the upload goes into (`_refusesSubImage`); null pixels are INVALID_VALUE.
    texSubImage2D(target, level, xoffset, yoffset, width, height, format, type, pixels, srcOffset) {
        if (pixels !== undefined) {
            const webgl2 = this._isWebGL2();
            const view = pixels !== null && typeof pixels === "object" && ArrayBufferIsView(pixels);
            const fromBuffer = webgl2 && pixels !== null && typeof pixels !== "object";
            if (pixels !== null && !view && !fromBuffer && !webgl2) {
                throw new TypeError("texSubImage2D: pixels is not an ArrayBufferView");
            }
            const source = pixels !== null && typeof pixels === "object" && !view
                ? requireTexImageSource(pixels, "texSubImage2D") : null;
            const texture = this._textureFor(target, "image2D");
            if (!texture) return;
            if (fromBuffer ? this._refusesUnpackBufferSource() : this._refusesUnpackBufferBound()) return;
            const formatError = source !== null
                ? this._sourceSubUploadFormatError(format, type) : this._subUploadFormatError(format, type);
            if (!preflightTexSubImage(
                this._canvasId, level, MAX_WEBGL_GPU_2D_LEVELS, formatError, xoffset, yoffset, 0, width, height, 1,
            )) return;
            if (source !== null) {
                if (this._refusesSourceSelection(source, width, height, 1, false) ||
                    this._refusesSubImage(texture, target, level, format, type, xoffset, yoffset, 0, width, height, 1)) return;
                settleCanvasTextCache(this, source, false);
                texImageFromSourceOf(
                    this, TEX_SOURCE_CALL_SUB_IMAGE_2D, target, level, 0, xoffset, yoffset, 0, width, height, 1,
                    format, type, _sourceCopyFormat(this._image(texture, Number(target) >>> 0, level)), source,
                );
                return;
            }
            if (fromBuffer) {
                const offset = this._unpackBufferOffset(pixels, width, height, 1, format, type, false);
                if (offset < 0 ||
                    this._refusesSubImage(texture, target, level, format, type, xoffset, yoffset, 0, width, height, 1)) return;
                _rawTexSubImage2D(
                    this._canvasId, target, level, xoffset, yoffset, width, height, format, type,
                    EMPTY_UPLOAD_BYTES, offset,
                );
                return;
            }
            if (pixels === null) {
                recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
                return;
            }
            if (this._refusesUnpackRegion(width, height, false)) return;
            const data = this._uploadViewBytes(pixels, webgl2 ? srcOffset : 0, width, height, 1, format, type, false);
            if (data === null ||
                this._refusesSubImage(texture, target, level, format, type, xoffset, yoffset, 0, width, height, 1)) return;
            _rawTexSubImage2D(this._canvasId, target, level, xoffset, yoffset, width, height, format, type, data, -1);
            return;
        }

        const source = requireTexImageSource(format, "texSubImage2D");
        const sourceFormat = width;
        const sourceType = height;
        const texture = this._textureFor(target, "image2D");
        if (!texture) return;
        if (this._refusesUnpackBufferBound()) return;
        if (!preflightTexSubImage(
            this._canvasId, level, MAX_WEBGL_GPU_2D_LEVELS, this._sourceSubUploadFormatError(sourceFormat, sourceType),
            xoffset, yoffset, 0, source.width, source.height, 1,
        )) return;
        if (this._refusesSourceSelection(source, source.width, source.height, 1, false) ||
            this._refusesSubImage(
                texture, target, level, sourceFormat, sourceType, xoffset, yoffset, 0, source.width, source.height, 1,
            )) return;
        settleCanvasTextCache(this, source, false);
        texImageFromSourceOf(
            this, TEX_SOURCE_CALL_SUB_IMAGE_2D, target, level, 0, xoffset, yoffset, 0, source.width, source.height, 1,
            sourceFormat, sourceType, _sourceCopyFormat(this._image(texture, Number(target) >>> 0, level)), source,
        );
    }

    // Whether mipmaps can be made of `image`: uncompressed, and a core unsized upload, or of a sized format both
    // colour-renderable and filterable -- `_MIPMAPPABLE_SIZED_FORMATS`, or a float one while an extension makes it
    // renderable (`floatColourRenderable`) and it is filterable (`_unfilterable`). WebGL 1's RGBA and RGB float images
    // are their sized formats (`_effectiveFormat`) under the same rule, so a luminance or alpha float one never is, as
    // no framebuffer renders it; its sRGB and depth ones are neither unsized uploads nor sized formats, so never, as
    // EXT_sRGB and WEBGL_depth_texture have it.
    _mipmappable(image) {
        if (image.compressed) return false;
        const i = image.internalformat;
        const unsized = _UNSIZED_UPLOAD_TYPES.get(i);
        if ((unsized !== undefined && _listHas(unsized, image.type)) || _listHas(_MIPMAPPABLE_SIZED_FORMATS, i)) return true;
        const sized = _effectiveFormat(image);
        const info = sized === undefined ? undefined : _FORMAT_INFO.get(sized);
        return info !== undefined && (info[7] & _FLOAT_RENDERABLE) !== 0 && floatColourRenderable(this, info) &&
            !_unfilterable(this, image);
    }

    // ---- Texture parameters ----------------------------------------------------------------------------------------
    // The parameter `pname` of the context's textures (`_TEXTURE_PARAMETERS`; WebGL 1 has four), or undefined.
    _textureParameter(pname) {
        if (pname === 0x84fe) return this._extTextureFilterAnisotropic === undefined ? undefined : _ANISOTROPY_PARAMETER;
        return this._isWebGL2() || _listHas(_WEBGL1_TEXTURE_PARAMETERS, pname) ? _TEXTURE_PARAMETERS.get(pname) : undefined;
    }

    // A sampler's parameter `pname` (`_SAMPLER_PARAMETERS`, and TEXTURE_MAX_ANISOTROPY_EXT once
    // EXT_texture_filter_anisotropic is enabled), or undefined.
    _samplerParameter(pname) {
        if (pname === 0x84fe) return this._extTextureFilterAnisotropic === undefined ? undefined : _ANISOTROPY_PARAMETER;
        return _SAMPLER_PARAMETERS.get(pname);
    }

    // A value TEXTURE_MAX_ANISOTROPY_EXT does not take: below 1 or past MAX_TEXTURE_MAX_ANISOTROPY_EXT.
    _refusesAnisotropy(pname, value) {
        return pname === 0x84fe && !(value >= 1 && value <= this._maxAnisotropy());
    }

    // `texParameteri` / `texParameterf` on the bound texture: a parameter the context's textures do not have, or a value
    // an enum one does not take, is INVALID_ENUM; a negative TEXTURE_BASE_LEVEL or TEXTURE_MAX_LEVEL INVALID_VALUE. The
    // value is recorded for `getTexParameter` and true returned; false when refused, with the error recorded.
    _setTexParameter(texture, pname, value) {
        const spec = this._textureParameter(pname);
        let error = 0;
        if (spec === undefined || (spec.values !== null && !_listHas(spec.values, value))) error = GL_INVALID_ENUM;
        else if (((pname === 0x813c || pname === 0x813d) && value < 0) || this._refusesAnisotropy(pname, value)) {
            error = GL_INVALID_VALUE;
        }
        if (error !== 0) {
            recordGpuPreflightError(this._canvasId, error);
            return false;
        }
        (texture._params || (texture._params = new Map())).set(pname, value);
        // MAG_FILTER, MIN_FILTER, WRAP_S, WRAP_T, TEXTURE_BASE_LEVEL
        if ((pname >= 0x2800 && pname <= 0x2803) || pname === 0x813c) refreshTextureSampling(this, texture);
        // TEXTURE_BASE_LEVEL, TEXTURE_MAX_LEVEL: which of its levels attach (`_attachableLevel`).
        if (pname === 0x813c || pname === 0x813d) framebufferChanged();
        return true;
    }

    // What `texParameter*` set, or the parameter's initial value; TEXTURE_IMMUTABLE_FORMAT and TEXTURE_IMMUTABLE_LEVELS
    // (WebGL 2) are what `texStorage*` made of the texture. A parameter the context's textures do not have is
    // INVALID_ENUM and null.
    getTexParameter(target, pname) {
        const texture = this._textureFor(target, "object");
        if (!texture) return null;
        const p = Number(pname) >>> 0;
        if (this._isWebGL2()) {
            if (p === 0x912f) return texture._immutableLevels !== 0;    // TEXTURE_IMMUTABLE_FORMAT
            if (p === 0x82df) return texture._immutableLevels;          // TEXTURE_IMMUTABLE_LEVELS
        }
        const spec = this._textureParameter(p);
        if (spec === undefined) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return null;
        }
        const value = texture._params === null ? undefined : texture._params.get(p);
        return value === undefined ? spec.initial : value;
    }

    texParameteri(target, pname, param) {
        const texture = this._textureFor(target, "object");
        if (!texture) return;
        const p = Number(pname) >>> 0;
        const value = Number(param) | 0;
        if (!this._setTexParameter(texture, p, value)) return;
        // opcode 40: H C U U I. target/pname are u32, param is i32: the arguments as WebIDL converted them.
        encodeTexParameteri(this._canvasId, Number(target) >>> 0, p,
            p === 0x2800 || p === 0x2801 ? _driverFilter(texture, p, value) : value);
    }

    // An enum or a level set through the float call takes the nearest integer (ES 3.0 2.3.1); the LODs are floats.
    texParameterf(target, pname, param) {
        const texture = this._textureFor(target, "object");
        if (!texture) return;
        const p = Number(pname) >>> 0;
        const f = MathFround(Number(param));
        if (!this._setTexParameter(texture, p, p === 0x813a || p === 0x813b || p === 0x84fe ? f : MathRound(f))) return;
        // opcode 41: H C U U F. target/pname are u32, param is f32.
        encodeTexParameterf(this._canvasId, Number(target) >>> 0, p,
            p === 0x2800 || p === 0x2801 ? _driverFilter(texture, p, MathRound(f)) : f);
    }

    // The base image (`_baseLevel`) must be there and not empty -- on every face of a cube map, alike and square --,
    // of a format mipmaps can be made of (`_mipmappable`), and in WebGL 1 a power of two each way; anything else is
    // INVALID_OPERATION (ES 3.0 3.8.10, ES 2.0 3.7.11). The levels it makes are recorded: each half the one before, down to 1 x 1 or the
    // maximum level (`_maxLevel`), as immutable storage already has them.
    generateMipmap(target) {
        const texture = this._textureFor(target, "object");
        if (!texture) return;
        const t = Number(target) >>> 0;
        const first = t === 0x8513 ? 0x8515 : t;
        const last = t === 0x8513 ? 0x851a : t;
        const base = this._baseLevel(texture);
        const image = this._image(texture, first, base);
        let ok = image !== undefined && image.width > 0 && image.height > 0 && image.depth > 0 && this._mipmappable(image) &&
            (this._isWebGL2() || (_isPowerOfTwo(image.width) && _isPowerOfTwo(image.height))) &&
            (t !== 0x8513 || image.width === image.height);
        for (let face = first + 1; ok && face <= last; face++) {
            const other = this._image(texture, face, base);
            ok = other !== undefined && other.width === image.width && other.height === image.height &&
                other.internalformat === image.internalformat && other.type === image.type;
        }
        if (!ok) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return;
        }
        // opcode 42: H C U.
        encodeGenerateMipmap(this._canvasId, t);
        if (texture._immutableLevels !== 0) return;
        const top = MathMin(this._maxLevel(texture),
            base + maxMipLevels(MathMax(image.width, image.height, t === 0x806f ? image.depth : 1)) - 1);
        for (let level = base + 1; level <= top; level++) {
            const shift = level - base;
            const mip = new TextureImage(image.internalformat, image.format, image.type,
                (image.width >> shift) || 1, (image.height >> shift) || 1,
                t === 0x806f ? (image.depth >> shift) || 1 : image.depth, false);
            for (let face = first; face <= last; face++) defineTextureImage(texture, face, level, mip);
        }
    }

    // A pname this context does not have is INVALID_ENUM; an alignment other than 1, 2, 4 or 8, or a negative length
    // or skip, INVALID_VALUE; a colour-space conversion other than NONE or BROWSER_DEFAULT_WEBGL, INVALID_ENUM. Nothing
    // refused is recorded or sent. UNPACK_FLIP_Y_WEBGL / UNPACK_PREMULTIPLY_ALPHA_WEBGL are WebGL's, not the driver's:
    // they go down the stream like the others and the renderer applies them to the pixels of each upload.
    pixelStorei(pname, param) {
        const p = Number(pname) >>> 0;
        const value = param === true ? 1 : param === false ? 0 : Number(param) | 0;
        let error = 0;
        if (p !== 0x9240 && p !== 0x9241) {
            if (!this._pixelStore.has(p)) error = GL_INVALID_ENUM;
            else if (p === 0x0d05 || p === 0x0cf5) error = value === 1 || value === 2 || value === 4 || value === 8 ? 0 : GL_INVALID_VALUE;
            else if (p === 0x9243) error = value === 0 || value === 0x9244 ? 0 : GL_INVALID_ENUM;
            else if (value < 0) error = GL_INVALID_VALUE;
        }
        if (error !== 0) {
            recordGpuPreflightError(this._canvasId, error);
            return;
        }
        if (p === 0x9240) this._unpackFlipY = value !== 0;
        else if (p === 0x9241) this._unpackPremultiplyAlpha = value !== 0;
        else this._pixelStore.set(p, value);
        // opcode 43: H C U I. pname is u32, value is i32.
        if (typeof pname === "number") {
            encodePixelStorei(this._canvasId, pname >>> 0, value);
            return;
        }
        flushRenderCommandStream();
        _rawPixelStorei(this._canvasId, pname, value);
    }

    // The compressed uploads are refused before anything is sent as `_acceptsCompressedImage` and
    // `_acceptsCompressedSubImage` describe; the image is recorded once one is sent.
    compressedTexImage2D(target, level, internalformat, width, height, border, data) {
        const texture = this._textureFor(target, "image2D");
        if (!texture) return;
        const u8 = toBoundedUploadBytes(this._canvasId, data);
        if (u8 === null || !this._acceptsCompressedImage(
            texture, target, level, internalformat, width, height, 1, border, TypedArrayPrototypeGetByteLength(u8),
        )) return;
        _rawCompressedTexImage2D(this._canvasId, target, level, internalformat, width, height, border, u8, -1, 0);
        defineCompressedTextureImage(texture, target, level, internalformat, width, height, 1);
        refreshTextureSampling(this, texture);
    }

    compressedTexSubImage2D(target, level, xoffset, yoffset, width, height, format, data) {
        const texture = this._textureFor(target, "image2D");
        if (!texture) return;
        const u8 = toBoundedUploadBytes(this._canvasId, data);
        if (u8 === null || !this._acceptsCompressedSubImage(
            texture, target, level, xoffset, yoffset, 0, width, height, 1, format, TypedArrayPrototypeGetByteLength(u8),
        )) return;
        _rawCompressedTexSubImage2D(this._canvasId, target, level, xoffset, yoffset, width, height, format, u8, -1, 0);
    }


    // -- Phase 1C: Buffer & Vertex Extensions --

    // `bufferSubData(target, dstByteOffset, data)`, and WebGL 2's with `srcOffset` and `length` counted in the view's
    // elements. The bytes must fall inside the bound buffer: a negative offset or a range past its size is
    // INVALID_VALUE (the driver's error would not reach `getError`), no buffer bound INVALID_OPERATION.
    bufferSubData(target, dstByteOffset, data, srcOffset, length) {
        const t = Number(target) >>> 0;
        const bound = this._boundBuffer(t);
        if (bound === undefined) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return;
        }
        if (bound === null) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return;
        }
        if (data === null || data === undefined) {
            throw new TypeError("Failed to execute 'bufferSubData' on 'WebGLRenderingContext': the source is not a BufferSource.");
        }
        const offset = toLongLong(Number(dstByteOffset));
        const u8 = arguments.length > 3 && this._isWebGL2()
            ? this._viewArgumentBytes("bufferSubData", data, srcOffset, length)
            : toBoundedUploadBytes(this._canvasId, data);
        if (u8 === null) return;
        if (offset < 0 || offset + TypedArrayPrototypeGetByteLength(u8) > (bound._size || 0)) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
            return;
        }
        if (bound._webglType === "element") {
            TypedArrayPrototypeSet(bound._elements, u8, offset);
            elementBytesChanged(bound);
        }
        _rawBufferSubData(this._canvasId, t, offset, u8);
    }

    disableVertexAttribArray(index) {
        // opcode 17: H C U.
        if (typeof index === "number") {
            encodeDisableVertexAttribArray(this._canvasId, index >>> 0);
        } else {
            flushRenderCommandStream();
            _rawDisableVertexAttribArray(this._canvasId, index);
        }
        const i = Number(index) >>> 0;
        if (i < _ATTRIB_SHADOW_SLOTS) this._attribShadow.enabled[i] = 0;
    }

    // ---- Constant vertex attributes ---------------------------------------------------------------------------------
    // What an attribute reads while its array is disabled. All four arities cross as one record: a call that gives
    // fewer than four components leaves the rest at 0, 0, 0, 1 as the specification has them.
    _vertexAttribF(index, x, y, z, w) {
        if (typeof index === "number" && typeof x === "number" && typeof y === "number" &&
            typeof z === "number" && typeof w === "number") {
            encodeVertexAttrib4f(this._canvasId, index, x, y, z, w);
        } else {
            flushRenderCommandStream();
            _rawVertexAttrib4f(this._canvasId, index, x, y, z, w);
        }
        const i = Number(index) >>> 0;
        if (i < _ATTRIB_SHADOW_SLOTS) {
            const f = this._currentAttribF, k = i * 4;
            f[k] = Number(x); f[k + 1] = Number(y); f[k + 2] = Number(z); f[k + 3] = Number(w);
            this._currentAttribKind[i] = 0;
        }
    }
    // The values of a `Float32List` / `Int32List` / `Uint32List` call: a typed array or a sequence, at least `n` long.
    // Anything that is not one is a TypeError; one that is too short is INVALID_VALUE and the call is ignored.
    _attribList(name, list, n) {
        if (list === null || typeof list !== "object" || typeof list.length !== "number") {
            throw new TypeError(`Failed to execute '${name}' on 'WebGLRenderingContext': parameter 2 is not of type '${list === undefined ? "undefined" : "list"}'.`);
        }
        if (list.length < n) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
            return false;
        }
        return true;
    }
    vertexAttrib1f(index, x) { this._vertexAttribF(index, x, 0, 0, 1); }
    vertexAttrib2f(index, x, y) { this._vertexAttribF(index, x, y, 0, 1); }
    vertexAttrib3f(index, x, y, z) { this._vertexAttribF(index, x, y, z, 1); }
    vertexAttrib4f(index, x, y, z, w) { this._vertexAttribF(index, x, y, z, w); }
    vertexAttrib1fv(index, v) { if (this._attribList("vertexAttrib1fv", v, 1)) this._vertexAttribF(index, v[0], 0, 0, 1); }
    vertexAttrib2fv(index, v) { if (this._attribList("vertexAttrib2fv", v, 2)) this._vertexAttribF(index, v[0], v[1], 0, 1); }
    vertexAttrib3fv(index, v) { if (this._attribList("vertexAttrib3fv", v, 3)) this._vertexAttribF(index, v[0], v[1], v[2], 1); }
    vertexAttrib4fv(index, v) { if (this._attribList("vertexAttrib4fv", v, 4)) this._vertexAttribF(index, v[0], v[1], v[2], v[3]); }

    // ---- Vertex attribute queries -----------------------------------------------------------------------------------
    // A device limit, asked once the context can answer and kept from then on. A context that cannot answer now
    // (lost) is held to `minimum`, what every implementation of the interface has, and asked again by the next call.
    // Callers compare against `minimum` first: an index below it needs no answer, so the common call never crosses.
    // A limit the driver answers, as a number, or null when it answers none (a lost context). Not `getParameter`: a
    // limit the facade reads is no content's query, and records no error.
    _driverParameter(pname) {
        return _typedParameter(_rawGetParameter(this._canvasId, pname), _P_NUMBER);
    }

    _cachedLimit(field, pname, minimum) {
        if (this[field] === 0) {
            const n = this._driverParameter(pname);
            if (!NumberIsInteger(n) || n < 1) return minimum;
            this[field] = n;
        }
        return this[field];
    }
    // Whether `index` names a vertex attribute: below MAX_VERTEX_ATTRIBS, which is at least 8 in WebGL 1 and 16 in
    // WebGL 2, and held to the slots the shadow keeps.
    _isAttribIndex(index) {
        if (index < this._attribMinimum) return true;
        return index < _ATTRIB_SHADOW_SLOTS && index < this._cachedLimit("_maxVertexAttribs", 0x8869, this._attribMinimum);
    }
    getVertexAttrib(index, pname) {
        const i = Number(index) >>> 0;
        if (!this._isAttribIndex(i)) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
            return null;
        }
        const sh = this._attribShadow;
        switch (pname) {
            case 0x8622: return sh.enabled[i] === 1;                      // VERTEX_ATTRIB_ARRAY_ENABLED
            case 0x8623: return sh.size[i];                               // VERTEX_ATTRIB_ARRAY_SIZE
            case 0x8624: return sh.stride[i];                             // VERTEX_ATTRIB_ARRAY_STRIDE
            case 0x8625: return sh.type[i];                               // VERTEX_ATTRIB_ARRAY_TYPE
            case 0x886a: return sh.normalized[i] === 1;                   // VERTEX_ATTRIB_ARRAY_NORMALIZED
            case 0x889f: return sh.buffer[i];                             // VERTEX_ATTRIB_ARRAY_BUFFER_BINDING
            case 0x8626: {                                                // CURRENT_VERTEX_ATTRIB
                const k = i * 4;
                const view = this._currentAttribKind[i] === 1 ? this._currentAttribI
                    : this._currentAttribKind[i] === 2 ? this._currentAttribU : this._currentAttribF;
                return view.slice(k, k + 4);
            }
            case 0x88fd:                                                  // VERTEX_ATTRIB_ARRAY_INTEGER (WebGL 2)
                if (this._isWebGL2()) return sh.integer[i] === 1;
                break;
            case 0x88fe:                                                  // VERTEX_ATTRIB_ARRAY_DIVISOR (WebGL 2, ANGLE_instanced_arrays)
                if (this._isWebGL2() || !!this._angleInstancedArrays) return sh.divisor[i];
                break;
            default: break;
        }
        recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
        return null;
    }
    getVertexAttribOffset(index, pname) {
        const i = Number(index) >>> 0;
        if (pname !== 0x8645) {                                            // VERTEX_ATTRIB_ARRAY_POINTER
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return 0;
        }
        if (!this._isAttribIndex(i)) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
            return 0;
        }
        return this._attribShadow.offset[i];
    }
    _isWebGL2() {
        return this._webgl2;
    }
    _bindAttribShadow(vao) {
        this._attribShadow = vao ? (vao._attribs || (vao._attribs = new VertexAttribShadow())) : this._attribDefaults;
    }

    // ---- Vertex array objects: WebGL 2's, and WebGL 1's through OES_vertex_array_object ----------------------------
    // One implementation; a WebGL 1 context reaches it only through the extension object, so it does not grow WebGL 2
    // methods content tells the two apart by.
    _createVertexArray() {
        const id = nextResourceId();
        _rawCreateVertexArray(this._canvasId, id);
        return new WebglObject(id, "vertexArray", this._canvasId);
    }
    _checkVertexArray(method, vao) {
        if (!(vao instanceof WebglObject) || vao._kind !== "vertexArray") {
            throw new TypeError(`Failed to execute '${method}': parameter 1 is not of type 'WebGLVertexArrayObject'.`);
        }
    }
    // Deleting the bound object binds the default one, as GL does.
    _deleteVertexArray(method, vao) {
        if (vao === null || vao === undefined) return;
        this._checkVertexArray(method, vao);
        if (vao._ownerId !== this._canvasId) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return;
        }
        if (vao._deleted) return;
        vao._deleted = true;
        _rawDeleteVertexArray(vao._id);
        if (this._vertexArrayBinding === vao) {
            this._vertexArrayBinding = null;
            this._bindAttribShadow(null);
        }
    }
    // A vertex array object is one once it has been bound, as `glIsVertexArray` answers.
    _isVertexArray(vao) {
        return this._isLive(vao, "vertexArray") && vao._everBound === true;
    }
    _bindVertexArray(method, vao) {
        const bound = vao === undefined ? null : vao;
        if (bound !== null) {
            this._checkVertexArray(method, bound);
            if (bound._deleted || bound._ownerId !== this._canvasId) {
                recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
                return;
            }
            bound._everBound = true;
        }
        // opcode 14: H C U. 0 binds the default object.
        encodeBindVertexArray(this._canvasId, bound ? bound._id >>> 0 : 0);
        this._vertexArrayBinding = bound;
        this._bindAttribShadow(bound);
    }

    clearDepth(depth) {
        // opcode 4: H C F.
        if (typeof depth === "number") {
            encodeClearDepth(this._canvasId, depth);
            return;
        }
        _rawClearDepth(this._canvasId, depth);
    }

    clearStencil(s) {
        // opcode 5: H C I.
        if (typeof s === "number") {
            encodeClearStencil(this._canvasId, s | 0);
            return;
        }
        flushRenderCommandStream();
        _rawClearStencil(this._canvasId, s);
    }

    // -- Phase 2A: Blend/Depth/Stencil/Cull State --

    // WebGL 1 takes SRC_ALPHA_SATURATE as a source factor only (ES 2.0 4.1.6): as a destination one it is INVALID_ENUM,
    // judged here because it depends on the version. What else the factors may be is the decoder's to judge
    // (`validate_blend_func`), for the stream and the raw call alike.
    _refusesDestinationFactor(dst, dstAlpha) {
        if (this._isWebGL2() || ((Number(dst) >>> 0) !== 0x0308 &&
                (dstAlpha === undefined || (Number(dstAlpha) >>> 0) !== 0x0308))) return false;
        recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
        return true;
    }
    blendFunc(sfactor, dfactor) {
        if (this._refusesDestinationFactor(dfactor, undefined)) return;
        if (typeof sfactor === "number" && typeof dfactor === "number") {
            encodeBlendFunc(this._canvasId, sfactor >>> 0, dfactor >>> 0);
        } else {
            flushRenderCommandStream();
            _rawBlendFunc(this._canvasId, sfactor, dfactor);
        }
    }
    blendFuncSeparate(srcRGB, dstRGB, srcAlpha, dstAlpha) {
        if (this._refusesDestinationFactor(dstRGB, dstAlpha)) return;
        if (typeof srcRGB === "number" && typeof dstRGB === "number" &&
            typeof srcAlpha === "number" && typeof dstAlpha === "number") {
            encodeBlendFuncSeparate(this._canvasId, srcRGB >>> 0, dstRGB >>> 0, srcAlpha >>> 0, dstAlpha >>> 0);
        } else {
            flushRenderCommandStream();
            _rawBlendFuncSeparate(this._canvasId, srcRGB, dstRGB, srcAlpha, dstAlpha);
        }
    }
    // FUNC_ADD, FUNC_SUBTRACT and FUNC_REVERSE_SUBTRACT; MIN and MAX in WebGL 2 or with EXT_blend_minmax enabled.
    // Anything else is INVALID_ENUM, before anything is sent: the driver takes MIN and MAX in either version.
    _blendEquationError(mode) {
        switch (Number(mode) >>> 0) {
            case 0x8006: case 0x800a: case 0x800b: return 0;
            case 0x8007: case 0x8008: return this._isWebGL2() || this._extBlendMinmax !== undefined ? 0 : GL_INVALID_ENUM;
            default: return GL_INVALID_ENUM;
        }
    }
    blendEquation(mode) {
        const error = this._blendEquationError(mode);
        if (error !== 0) {
            recordGpuPreflightError(this._canvasId, error);
            return;
        }
        if (typeof mode === "number") {
            encodeBlendEquation(this._canvasId, mode >>> 0);
        } else {
            flushRenderCommandStream();
            _rawBlendEquation(this._canvasId, mode);
        }
    }
    blendEquationSeparate(modeRGB, modeAlpha) {
        const error = this._blendEquationError(modeRGB) || this._blendEquationError(modeAlpha);
        if (error !== 0) {
            recordGpuPreflightError(this._canvasId, error);
            return;
        }
        if (typeof modeRGB === "number" && typeof modeAlpha === "number") {
            encodeBlendEquationSeparate(this._canvasId, modeRGB >>> 0, modeAlpha >>> 0);
        } else {
            flushRenderCommandStream();
            _rawBlendEquationSeparate(this._canvasId, modeRGB, modeAlpha);
        }
    }
    blendColor(r, g, b, a) {
        if (typeof r === "number" && typeof g === "number" &&
            typeof b === "number" && typeof a === "number") {
            encodeBlendColor(this._canvasId, r, g, b, a);
        } else {
            _rawBlendColor(this._canvasId, r, g, b, a);
        }
    }
    depthFunc(func) {
        if (typeof func === "number") {
            encodeDepthFunc(this._canvasId, func >>> 0);
        } else {
            flushRenderCommandStream();
            _rawDepthFunc(this._canvasId, func);
        }
    }
    depthMask(flag) {
        if (typeof flag === "boolean") {
            encodeDepthMask(this._canvasId, flag);
        } else {
            flushRenderCommandStream();
            op_depth_mask(this._canvasId, flag);
        }
    }
    depthRange(near, far) {
        if (typeof near === "number" && typeof far === "number") {
            encodeDepthRange(this._canvasId, near, far);
        } else {
            _rawDepthRange(this._canvasId, near, far);
        }
    }
    stencilFunc(func, ref_, mask) {
        if (_isComparison(func)) noteStencilMask(this, 0x0408, 0, mask);
        if (typeof func === "number" && typeof ref_ === "number" && typeof mask === "number") {
            encodeStencilFunc(this._canvasId, func >>> 0, ref_ | 0, mask >>> 0);
        } else {
            flushRenderCommandStream();
            _rawStencilFunc(this._canvasId, func, ref_, mask);
        }
    }
    stencilFuncSeparate(face, func, ref_, mask) {
        if (_isComparison(func)) noteStencilMask(this, face, 0, mask);
        if (typeof face === "number" && typeof func === "number" &&
            typeof ref_ === "number" && typeof mask === "number") {
            encodeStencilFuncSeparate(this._canvasId, face >>> 0, func >>> 0, ref_ | 0, mask >>> 0);
        } else {
            flushRenderCommandStream();
            _rawStencilFuncSeparate(this._canvasId, face, func, ref_, mask);
        }
    }
    stencilOp(fail, zfail, zpass) {
        if (typeof fail === "number" && typeof zfail === "number" && typeof zpass === "number") {
            encodeStencilOp(this._canvasId, fail >>> 0, zfail >>> 0, zpass >>> 0);
        } else {
            flushRenderCommandStream();
            _rawStencilOp(this._canvasId, fail, zfail, zpass);
        }
    }
    stencilOpSeparate(face, fail, zfail, zpass) {
        if (typeof face === "number" && typeof fail === "number" &&
            typeof zfail === "number" && typeof zpass === "number") {
            encodeStencilOpSeparate(this._canvasId, face >>> 0, fail >>> 0, zfail >>> 0, zpass >>> 0);
        } else {
            flushRenderCommandStream();
            _rawStencilOpSeparate(this._canvasId, face, fail, zfail, zpass);
        }
    }
    stencilMask(mask) {
        noteStencilMask(this, 0x0408, 2, mask);
        if (typeof mask === "number") {
            encodeStencilMask(this._canvasId, mask >>> 0);
        } else {
            flushRenderCommandStream();
            _rawStencilMask(this._canvasId, mask);
        }
    }
    stencilMaskSeparate(face, mask) {
        noteStencilMask(this, face, 2, mask);
        if (typeof face === "number" && typeof mask === "number") {
            encodeStencilMaskSeparate(this._canvasId, face >>> 0, mask >>> 0);
        } else {
            flushRenderCommandStream();
            _rawStencilMaskSeparate(this._canvasId, face, mask);
        }
    }
    cullFace(mode) {
        if (typeof mode === "number") {
            encodeCullFace(this._canvasId, mode >>> 0);
        } else {
            flushRenderCommandStream();
            _rawCullFace(this._canvasId, mode);
        }
    }
    frontFace(mode) {
        if (typeof mode === "number") {
            encodeFrontFace(this._canvasId, mode >>> 0);
        } else {
            flushRenderCommandStream();
            _rawFrontFace(this._canvasId, mode);
        }
    }
    colorMask(r, g, b, a) {
        if (typeof r === "boolean" && typeof g === "boolean" &&
            typeof b === "boolean" && typeof a === "boolean") {
            encodeColorMask(this._canvasId, r, g, b, a);
        } else {
            flushRenderCommandStream();
            op_color_mask(this._canvasId, r, g, b, a);
        }
    }
    scissor(x, y, width, height) {
        // opcode 37: H C I I I I.
        if (typeof x === "number" && typeof y === "number" &&
            typeof width === "number" && typeof height === "number") {
            encodeScissor(this._canvasId, x | 0, y | 0, width | 0, height | 0);
        } else {
            flushRenderCommandStream();
            _rawScissor(this._canvasId, x, y, width, height);
        }
    }
    lineWidth(width) {
        if (typeof width === "number") {
            encodeLineWidth(this._canvasId, width);
        } else {
            _rawLineWidth(this._canvasId, width);
        }
    }
    polygonOffset(factor, units) {
        if (typeof factor === "number" && typeof units === "number") {
            encodePolygonOffset(this._canvasId, factor, units);
        } else {
            _rawPolygonOffset(this._canvasId, factor, units);
        }
    }

    // -- Phase 2B: Uniform Variants --

    uniform1i(location, x) {
        // WebGL coerces the value (WebIDL GLint) -- engines pass booleans for
        // `uniform bool` samplers/flags (e.g. Phaser: `uniform1i(loc, true)`).
        // `| 0` applies ToInt32 (true -> 1, false -> 0), matching the browser.
        // opcode 54: H C I I. location is i32, x is i32.
        const loc = _loc(location);
        const xi = x | 0;
        encodeUniform1i(this._canvasId, loc, xi);
    }
    uniform1f(location, x) {
        // opcode 55: H C I F. Always encodable (f32 accepts any number).
        encodeUniform1f(this._canvasId, _loc(location), +x);
    }
    uniform2f(location, x, y) {
        // opcode 56: H C I F F.
        encodeUniform2f(this._canvasId, _loc(location), +x, +y);
    }
    uniform4f(location, x, y, z, w) {
        // opcode 58: H C I F F F F.
        encodeUniform4f(this._canvasId, _loc(location), +x, +y, +z, +w);
    }
    // The integer vector setters with their components as arguments (WebGL 1: `ivec2`/`ivec3`/`ivec4` and
    // `bvec` uniforms). Phaser sets its integer uniforms through them. Each is the array form of the same width,
    // so the record, the validation and the raw fallback are the ones `uniformNiv` already has; `| 0` is the
    // ToInt32 WebIDL's GLint applies, and storing it in a Uint32Array keeps its bits.
    uniform2i(location, x, y) {
        const loc = _loc(location);
        const payload = this._uniformI32Scratch[2];
        payload[0] = x | 0;
        payload[1] = y | 0;
        if (!encodeUniform2iv(this._canvasId, loc, payload)) {
            flushRenderCommandStream();
            _rawUniform2iv(this._canvasId, loc, payload);
        }
    }
    uniform3i(location, x, y, z) {
        const loc = _loc(location);
        const payload = this._uniformI32Scratch[3];
        payload[0] = x | 0;
        payload[1] = y | 0;
        payload[2] = z | 0;
        if (!encodeUniform3iv(this._canvasId, loc, payload)) {
            flushRenderCommandStream();
            _rawUniform3iv(this._canvasId, loc, payload);
        }
    }
    uniform4i(location, x, y, z, w) {
        const loc = _loc(location);
        const payload = this._uniformI32Scratch[4];
        payload[0] = x | 0;
        payload[1] = y | 0;
        payload[2] = z | 0;
        payload[3] = w | 0;
        if (!encodeUniform4iv(this._canvasId, loc, payload)) {
            flushRenderCommandStream();
            _rawUniform4iv(this._canvasId, loc, payload);
        }
    }
    uniform1iv(location, value) {
        const loc = _loc(location);
        const payload = toInt32AsUint32(value);
        if (!encodeUniform1iv(this._canvasId, loc, payload)) {
            flushRenderCommandStream();
            _rawUniform1iv(this._canvasId, loc, payload);
        }
    }
    uniform1fv(location, value) {
        const loc = _loc(location);
        const payload = toFloat32AsUint32(value);
        if (!encodeUniform1fv(this._canvasId, loc, payload)) {
            flushRenderCommandStream();
            _rawUniform1fv(this._canvasId, loc, payload);
        }
    }
    uniform2iv(location, value) {
        const loc = _loc(location);
        const payload = toInt32AsUint32(value);
        if (!encodeUniform2iv(this._canvasId, loc, payload)) {
            flushRenderCommandStream();
            _rawUniform2iv(this._canvasId, loc, payload);
        }
    }
    uniform2fv(location, value) {
        const loc = _loc(location);
        const payload = toFloat32AsUint32(value);
        if (!encodeUniform2fv(this._canvasId, loc, payload)) {
            flushRenderCommandStream();
            _rawUniform2fv(this._canvasId, loc, payload);
        }
    }
    uniform3iv(location, value) {
        const loc = _loc(location);
        const payload = toInt32AsUint32(value);
        if (!encodeUniform3iv(this._canvasId, loc, payload)) {
            flushRenderCommandStream();
            _rawUniform3iv(this._canvasId, loc, payload);
        }
    }
    uniform3fv(location, value) {
        const loc = _loc(location);
        const payload = toFloat32AsUint32(value);
        if (!encodeUniform3fv(this._canvasId, loc, payload)) {
            flushRenderCommandStream();
            _rawUniform3fv(this._canvasId, loc, payload);
        }
    }
    uniform4iv(location, value) {
        const loc = _loc(location);
        const payload = toInt32AsUint32(value);
        if (!encodeUniform4iv(this._canvasId, loc, payload)) {
            flushRenderCommandStream();
            _rawUniform4iv(this._canvasId, loc, payload);
        }
    }
    uniform4fv(location, value) {
        const loc = _loc(location);
        const payload = toFloat32AsUint32(value);
        if (!encodeUniform4fv(this._canvasId, loc, payload)) {
            flushRenderCommandStream();
            _rawUniform4fv(this._canvasId, loc, payload);
        }
    }
    uniformMatrix2fv(location, transpose, value) {
        const loc = _loc(location);
        const payload = toFloat32AsUint32(value);
        if (typeof transpose !== "boolean" ||
            !encodeUniformMatrix2fv(this._canvasId, loc, transpose, payload)) {
            flushRenderCommandStream();
            op_uniform_matrix_2fv(this._canvasId, loc, transpose, payload);
        }
    }
    uniformMatrix4fv(location, transpose, value) {
        const loc = _loc(location);
        const payload = toFloat32AsUint32(value);
        if (typeof transpose !== "boolean" ||
            !encodeUniformMatrix4fv(this._canvasId, loc, transpose, payload)) {
            flushRenderCommandStream();
            op_uniform_matrix_4fv(this._canvasId, loc, transpose, payload);
        }
    }

    // -- Phase 3A: Framebuffer/Renderbuffer --

    createFramebuffer() {
        const id = nextResourceId();
        _rawCreateFramebuffer(this._canvasId, id);
        return new WebglObject(id, "framebuffer", this._canvasId);
    }
    // Whether `object` may be deleted: false for null or one already deleted, a TypeError for a value that is not of
    // `kind`, INVALID_OPERATION (and false) for another context's.
    _deletes(method, object, kind, type) {
        if (object === null || object === undefined) return false;
        if (!(object instanceof WebglObject) || object._kind !== kind) {
            throw new TypeError(`Failed to execute '${method}' on 'WebGLRenderingContext': parameter 1 is not of type '${type}'.`);
        }
        if (object._ownerId !== this._canvasId) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return false;
        }
        return !object._deleted;
    }
    // A deleted framebuffer that is bound leaves its binding to the default framebuffer (ES 3.0 4.4.1).
    deleteFramebuffer(fb) {
        if (!this._deletes("deleteFramebuffer", fb, "framebuffer", "WebGLFramebuffer")) return;
        fb._deleted = true;
        if (this._framebufferBinding === fb) this._framebufferBinding = null;
        if (this._readFramebufferBinding === fb) this._readFramebufferBinding = null;
        _rawDeleteFramebuffer(fb._id);
    }
    // Whether binding `object` of `kind` is refused: 0 for null or a live object of this context, INVALID_OPERATION
    // for a deleted one or another context's; a value that is not one is a TypeError. A bind that takes marks the
    // object as one (`isFramebuffer` and its siblings are false until then, as GL answers).
    _bindError(method, object, kind, type) {
        if (object === null) return 0;
        if (!(object instanceof WebglObject) || object._kind !== kind) {
            throw new TypeError(`Failed to execute '${method}' on 'WebGLRenderingContext': parameter 2 is not of type '${type}'.`);
        }
        return object._deleted || object._ownerId !== this._canvasId ? GL_INVALID_OPERATION : 0;
    }

    // FRAMEBUFFER binds both points; WebGL 2's READ_FRAMEBUFFER and DRAW_FRAMEBUFFER one each, and any other target is
    // INVALID_ENUM. Tracked here rather than asked of the driver because `getParameter` must return this context's own
    // wrapper object, not a name.
    bindFramebuffer(target, fb) {
        const t = Number(target) >>> 0;
        if (t !== 0x8d40 && !((t === 0x8ca8 || t === 0x8ca9) && this._isWebGL2())) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return;
        }
        const bound = fb === undefined ? null : fb;
        const error = this._bindError("bindFramebuffer", bound, "framebuffer", "WebGLFramebuffer");
        if (error !== 0) {
            recordGpuPreflightError(this._canvasId, error);
            return;
        }
        if (bound !== null) bound._everBound = true;
        if (t !== 0x8ca9) this._readFramebufferBinding = bound;      // FRAMEBUFFER, READ_FRAMEBUFFER
        if (t !== 0x8ca8) this._framebufferBinding = bound;          // FRAMEBUFFER, DRAW_FRAMEBUFFER
        const fbId = bound ? bound._id : -1;
        // opcode 12: H C U I.
        if (typeof target === "number") {
            encodeBindFramebuffer(this._canvasId, t, fbId);
            return;
        }
        flushRenderCommandStream();
        _rawBindFramebuffer(this._canvasId, target, fbId);
    }
    // ---- Copies from the read framebuffer ----------------------------------------------------------------------------
    // The read framebuffer -- the drawing buffer, or the content's own -- into the bound texture, on the GPU: only the
    // arguments cross. The decoder checks what needs no state for both lanes (`validate_copy_tex_image_2d`); this side
    // checks the same, in the same order, and what the decoder cannot know (the levels and sizes the context takes, a
    // WebGL 1 mipmap that is not a power of two, immutable storage, the image a sub-copy goes into), so that the image
    // the copy defines is recorded only when the copy is sent. That the read framebuffer's format converts to the
    // texture's is the driver's to judge.
    copyTexImage2D(target, level, internalformat, x, y, width, height, border) {
        const texture = this._textureFor(target, "image2D");
        if (!texture) return;
        const t = Number(target) >>> 0;
        const i = Number(internalformat) >>> 0;
        const l = level | 0, w = width | 0, h = height | 0;
        const error = this._copyTexImageError(t, l, i, w, h, border | 0);
        if (error !== 0) {
            recordGpuPreflightError(this._canvasId, error);
            return;
        }
        if (this._refusesNpotLevel(l, w, h) || this._refusesImmutable(texture) || this._refusesCopyFrom(i)) return;
        encodeCopyTexImage2D(this._canvasId, t, l, i, x, y, w, h, 0);
        const sized = _SIZED_UPLOADS.get(i);
        defineTextureImage(texture, t, l, sized === undefined
            ? new TextureImage(i, i, _UBYTE, w, h, 1, false)
            : new TextureImage(i, sized[0], sized[1][0], w, h, 1, false));
        refreshTextureSampling(this, texture);
    }

    // A copy from the read framebuffer: one that is not complete is INVALID_FRAMEBUFFER_OPERATION, a read buffer that is
    // NONE or names no image -- or one the copy cannot convert into `internalformat` (`copyCompatible`) --
    // INVALID_OPERATION. True when refused, the error recorded.
    _refusesCopyFrom(internalformat) {
        if (refusesIncompleteFramebuffer(this, this._readFramebufferBinding)) return true;
        const source = readColorFormat(this);
        if (source !== 0 && copyCompatible(source, internalformat)) return false;
        recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
        return true;
    }

    // The internal format first (INVALID_ENUM for one the call does not take: WebGL 1 has the five unsized ones only),
    // then the level, size and border (INVALID_VALUE: out of range, or a cube face that is not square), then a depth or
    // stencil format (INVALID_OPERATION: WebGL 1's with WEBGL_depth_texture enabled, as it specifies), as the decoder
    // orders them. 0 when none is.
    _copyTexImageError(target, level, internalformat, width, height, border) {
        const formatError = this._isWebGL2() ? _copyTexImageFormatError(this, internalformat)
            : internalformat >= 0x1906 && internalformat <= 0x190a ? 0
            : (internalformat === 0x1902 || internalformat === 0x84f9) && this._webglDepthTexture !== undefined
                ? GL_INVALID_OPERATION : GL_INVALID_ENUM;
        if (formatError === GL_INVALID_ENUM) return formatError;
        const maxAtLevel = (MAX_WEBGL_GPU_2D_DIMENSION >>> level) || 1;
        if (level < 0 || level >= MAX_WEBGL_GPU_2D_LEVELS || width < 0 || width > maxAtLevel || height < 0 ||
                height > maxAtLevel || border !== 0 || (target !== 0x0de1 && width !== height)) {
            return GL_INVALID_VALUE;
        }
        return formatError;
    }

    copyTexSubImage2D(target, level, xoffset, yoffset, x, y, width, height) {
        const texture = this._textureFor(target, "image2D");
        if (!texture || this._refusesCopyIntoImage(
            texture, target, level | 0, xoffset | 0, yoffset | 0, 0, width | 0, height | 0,
        ) || this._refusesCopyFrom(this._copyDestination(this._image(texture, target, level | 0)))) return;
        encodeCopyTexSubImage2D(this._canvasId, target, level, xoffset, yoffset, x, y, width, height);
    }

    // The format a copy into `image` fills (`copyCompatible`): its internal format, but for WebGL 1's float and sRGB
    // images the sized format each is, as the driver has it -- RGBA32F and the like, R32F or R16F for luminance -- and
    // none for an alpha or luminance-alpha float one, whose alpha a copy cannot fill: the driver holds it in red or green.
    _copyDestination(image) {
        const i = image.internalformat;
        if (this._isWebGL2()) return i;
        const float = image.type === _FLOAT;
        if (float || image.type === 0x8d61) {
            if (i === 0x1909) return float ? 0x822e : 0x822d;                // LUMINANCE: R32F, R16F
            return i === 0x1906 || i === 0x190a ? 0 : _effectiveFormat(image);
        }
        return i === 0x8c40 || i === 0x8c42 ? _effectiveFormat(image) : i;  // SRGB_EXT, SRGB_ALPHA_EXT
    }

    // Refused before anything is sent (`_attachmentFramebuffer`): a textarget that is not TEXTURE_2D or a cube face is
    // INVALID_ENUM; a texture of another context, a deleted one, or one whose target is not the textarget's
    // INVALID_OPERATION; a level other than 0 in WebGL 1 without OES_fbo_render_mipmap, or past the texture's levels,
    // INVALID_VALUE.
    framebufferTexture2D(target, attachment, textarget, texture, level) {
        const tt = Number(textarget) >>> 0;
        const object = texture === undefined ? null : texture;
        const lv = level | 0;
        const fb = this._attachmentFramebuffer(target, attachment, () => {
            if (tt !== 0x0de1 && !(tt >= 0x8515 && tt <= 0x851a)) return GL_INVALID_ENUM;
            if (object !== null) {
                if (!(object instanceof WebglObject) || object._kind !== "texture") {
                    throw new TypeError("Failed to execute 'framebufferTexture2D' on 'WebGLRenderingContext': parameter 4 is not of type 'WebGLTexture'.");
                }
                if (object._deleted || object._ownerId !== this._canvasId ||
                        object._target !== (tt === 0x0de1 ? 0x0de1 : 0x8513)) return GL_INVALID_OPERATION;
            }
            if (lv < 0 || (this._isWebGL2() || this._oesFboRenderMipmap !== undefined ? lv >= this._levelLimit(tt)
                : lv !== 0)) return GL_INVALID_VALUE;
            return 0;
        });
        if (fb === undefined) return;
        this._noteAttachment(target, attachment, object ? { type: 0x1702, object, level: lv, face: tt } : null);
        _rawFramebufferTexture2D(this._canvasId, target, attachment, tt, object ? object.id : -1, lv);
    }
    // Refused before anything is sent (`_attachmentFramebuffer`): a renderbuffertarget other than RENDERBUFFER is
    // INVALID_ENUM; a renderbuffer of another context, a deleted one, or one never bound INVALID_OPERATION.
    framebufferRenderbuffer(target, attachment, renderbuffertarget, renderbuffer) {
        const object = renderbuffer === undefined ? null : renderbuffer;
        const fb = this._attachmentFramebuffer(target, attachment, () => {
            if ((Number(renderbuffertarget) >>> 0) !== 0x8d41) return GL_INVALID_ENUM;
            if (object === null) return 0;
            if (!(object instanceof WebglObject) || object._kind !== "renderbuffer") {
                throw new TypeError("Failed to execute 'framebufferRenderbuffer' on 'WebGLRenderingContext': parameter 4 is not of type 'WebGLRenderbuffer'.");
            }
            return object._deleted || object._ownerId !== this._canvasId || object._everBound !== true
                ? GL_INVALID_OPERATION : 0;
        });
        if (fb === undefined) return;
        this._noteAttachment(target, attachment, object ? { type: 0x8d41, object, level: 0, face: 0 } : null);
        _rawFramebufferRenderbuffer(this._canvasId, target, attachment, 0x8d41, object ? object.id : -1);
    }

    // What is attached where, recorded on the framebuffer it was attached to. `getFramebufferAttachmentParameter` answers the
    // object, its type and its level from here -- the driver's answer would be a GL name, not the wrapper the content holds --
    // and asks the driver only for what the facade cannot know (an attached texture's component sizes).
    // A deleted texture or renderbuffer leaves the framebuffers bound -- draw and read -- and keeps its place on the
    // others (ES 3.0 D.1.2).
    _detachFromBoundFramebuffers(object) {
        for (const fb of [this._framebufferBinding, this._readFramebufferBinding]) {
            if (!fb || !fb._attachments) continue;
            for (const [point, record] of fb._attachments) if (record.object === object) fb._attachments.delete(point);
        }
        framebufferChanged();
    }

    _noteAttachment(target, attachment, record) {
        const fb = target === 0x8ca8 ? this._readFramebufferBinding : this._framebufferBinding;
        if (!fb || !(fb instanceof WebglObject)) return;   // the default framebuffer takes no attachments
        if (!fb._attachments) fb._attachments = new Map();
        const a = attachment >>> 0;
        if (record) record.point = a;
        // DEPTH_STENCIL_ATTACHMENT is both of the others (ES 3.0 4.4.2): record it as both.
        const points = a === 0x821a ? [0x8d00, 0x8d20] : [a];
        for (const point of points) {
            if (record) fb._attachments.set(point, record); else fb._attachments.delete(point);
        }
        framebufferChanged();
    }

    // The framebuffer an attachment call changes, or undefined with the error recorded, in a browser's order: a target
    // that is not a framebuffer binding (WebGL 1 has FRAMEBUFFER only) or an attachment point the context does not
    // have is INVALID_ENUM -- in WebGL 1 a colour attachment past the first needs WEBGL_draw_buffers, in WebGL 2 one
    // past MAX_COLOR_ATTACHMENTS is INVALID_OPERATION --; then `objectError`, the attached object's; then the default
    // framebuffer bound to the target, which takes no attachments, INVALID_OPERATION.
    _attachmentFramebuffer(target, attachment, objectError) {
        const t = Number(target) >>> 0;
        let error = 0;
        if (t !== 0x8d40 && !((t === 0x8ca8 || t === 0x8ca9) && this._isWebGL2())) {
            error = GL_INVALID_ENUM;
        } else {
            const a = Number(attachment) >>> 0;
            const color = a - 0x8ce0;
            if (a !== 0x8d00 && a !== 0x8d20 && a !== 0x821a) {
                if (color < 0 || color >= 16) error = GL_INVALID_ENUM;
                else if (!this._hasColorAttachment(color)) error = this._isWebGL2() ? GL_INVALID_OPERATION : GL_INVALID_ENUM;
            }
        }
        if (error === 0) error = objectError();
        const fb = t === 0x8ca8 ? this._readFramebufferBinding : this._framebufferBinding;
        if (error === 0 && fb === null) error = GL_INVALID_OPERATION;
        if (error === 0) return fb;
        recordGpuPreflightError(this._canvasId, error);
        return undefined;
    }

    // Whether COLOR_ATTACHMENT`index` is one of the context's points. Below the least every implementation has -- four
    // in WebGL 2 and with WEBGL_draw_buffers, one otherwise -- without asking the limit.
    _hasColorAttachment(index) {
        const minimum = this._isWebGL2() || this._webglDrawBuffers !== undefined ? 4 : 1;
        return index < minimum || index < this._colorAttachmentLimit();
    }

    // A value that is not NONE, BACK or a colour attachment the context has is INVALID_ENUM; more values than
    // MAX_DRAW_BUFFERS INVALID_VALUE; for the default framebuffer anything but BACK or NONE alone, and for an object
    // anything but COLOR_ATTACHMENTi or NONE at place i, INVALID_OPERATION. 0 when none is.
    _drawBuffersError(list) {
        const n = list.length;
        for (let i = 0; i < n; i++) {
            const b = list[i];
            const color = b - 0x8ce0;
            if (b !== 0 && b !== 0x0405 && !(color >= 0 && color < 16 && this._hasColorAttachment(color))) {
                return GL_INVALID_ENUM;
            }
        }
        if (n > 4 && n > this._drawBufferLimit()) return GL_INVALID_VALUE;
        if (this._framebufferBinding === null) {
            return n === 1 && (list[0] === 0 || list[0] === 0x0405) ? 0 : GL_INVALID_OPERATION;
        }
        for (let i = 0; i < n; i++) {
            if (list[i] !== 0 && list[i] !== 0x8ce0 + i) return GL_INVALID_OPERATION;
        }
        return 0;
    }

    // How many colour attachment points the context has: MAX_COLOR_ATTACHMENTS in WebGL 2, WEBGL_draw_buffers' in
    // WebGL 1 once it is enabled, one otherwise.
    _colorAttachmentLimit() {
        if (this._isWebGL2() || this._webglDrawBuffers !== undefined) {
            return this._cachedLimit("_maxColorAttachments", 0x8cdf, 4);
        }
        return 1;
    }

    // In the order a browser judges a query: the target (FRAMEBUFFER, and WebGL 2's DRAW_FRAMEBUFFER and
    // READ_FRAMEBUFFER), the attachment point (a colour one past the context's is INVALID_ENUM), in WebGL 1 a framebuffer
    // bound (ES 2.0 6.1.3: the default one has no attachments to query, INVALID_OPERATION), then the pname -- WebGL 1
    // has the object's type, name, level and face, and COMPONENT_TYPE with WEBGL_color_buffer_float or
    // EXT_color_buffer_half_float and COLOR_ENCODING with EXT_sRGB enabled; COMPONENT_TYPE of DEPTH_STENCIL_ATTACHMENT
    // is INVALID_OPERATION, its two buffers' types may differ (ES 3.0 6.1.13).
    getFramebufferAttachmentParameter(target, attachment, pname) {
        const webgl2 = this._isWebGL2();
        if (target !== 0x8d40 && !(webgl2 && (target === 0x8ca9 || target === 0x8ca8))) {
            recordGpuPreflightError(this._canvasId, 0x0500);
            return null;
        }
        const fb = target === 0x8ca8 ? this._readFramebufferBinding : this._framebufferBinding;
        pname = pname >>> 0;
        attachment = attachment >>> 0;
        if (!fb && webgl2) return this._defaultFramebufferAttachmentParameter(attachment, pname);
        const color = attachment - 0x8ce0;
        const isColor = color >= 0 && color < this._colorAttachmentLimit();
        if (!isColor && attachment !== 0x8d00 && attachment !== 0x8d20 && attachment !== 0x821a) {
            recordGpuPreflightError(this._canvasId, 0x0500);
            return null;
        }
        if (!fb) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return null;
        }
        // DEPTH_STENCIL_ATTACHMENT answers only when one object is both: the same one in each.
        let record = null;
        if (attachment === 0x821a) {
            const depth = fb._attachments && fb._attachments.get(0x8d00);
            const stencil = fb._attachments && fb._attachments.get(0x8d20);
            if (depth && stencil && depth.object === stencil.object) record = depth;
            else if (depth || stencil) { recordGpuPreflightError(this._canvasId, 0x0506); return null; }
        } else {
            record = fb._attachments ? fb._attachments.get(attachment) || null : null;
        }
        const validPname = pname === 0x8cd0 || pname === 0x8cd1 || pname === 0x8cd2 || pname === 0x8cd3 || (webgl2
            ? pname === 0x8cd4 || (pname >= 0x8212 && pname <= 0x8217) || pname === 0x8211 || pname === 0x8210
            : (pname === 0x8211 && (this._webglColorBufferFloat !== undefined || this._extColorBufferHalfFloat !== undefined)) ||
                (pname === 0x8210 && this._extSrgb !== undefined));
        if (!validPname) { recordGpuPreflightError(this._canvasId, 0x0500); return null; }
        if (pname === 0x8211 && attachment === 0x821a) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return null;
        }
        if (!record) {
            if (pname === 0x8cd0) return 0;                        // OBJECT_TYPE: NONE
            if (pname === 0x8cd1) return null;                     // OBJECT_NAME: no object
            recordGpuPreflightError(this._canvasId, 0x0502);                             // anything else needs an object
            return null;
        }
        switch (pname) {
            case 0x8cd0: return record.type;                       // FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE
            case 0x8cd1: return record.object;                     // FRAMEBUFFER_ATTACHMENT_OBJECT_NAME: the wrapper
            case 0x8cd2: return record.type === 0x1702 ? record.level : this._invalidAttachmentQuery();      // TEXTURE_LEVEL
            case 0x8cd3: {                                         // TEXTURE_CUBE_MAP_FACE
                if (record.type !== 0x1702) return this._invalidAttachmentQuery();
                return record.face >= 0x8515 && record.face <= 0x851a ? record.face : 0;
            }
            case 0x8cd4: return record.type === 0x1702 ? (record.layer | 0) : this._invalidAttachmentQuery();  // TEXTURE_LAYER
            default: break;
        }
        // The component sizes, type and colour encoding. A renderbuffer's format is recorded here; a texture's is the
        // driver's to say.
        if (record.type === 0x8d41) {
            const format = record.object._format === undefined ? 0x8056 : record.object._format;
            switch (pname) {
                case 0x8212: return _renderbufferBits(format, 0);  // RED_SIZE
                case 0x8213: return _renderbufferBits(format, 1);  // GREEN_SIZE
                case 0x8214: return _renderbufferBits(format, 2);  // BLUE_SIZE
                case 0x8215: return _renderbufferBits(format, 3);  // ALPHA_SIZE
                case 0x8216: return _renderbufferBits(format, 4);  // DEPTH_SIZE
                case 0x8217: return _renderbufferBits(format, 5);  // STENCIL_SIZE
                case 0x8211: return _componentType(format);                                       // COMPONENT_TYPE
                case 0x8210: {                                                                     // COLOR_ENCODING
                    const info = _FORMAT_INFO.get(format);
                    return info !== undefined && (info[7] & _SRGB) !== 0 ? 0x8c40 : 0x2601;       // SRGB or LINEAR
                }
                default: return null;
            }
        }
        const json = _rawGetGlState(this._canvasId, GL_STATE_FRAMEBUFFER_ATTACHMENT_PARAMETER, target, attachment, String(pname));
        const value = Number(json);
        return Number.isFinite(value) ? value : null;
    }

    _invalidAttachmentQuery() {
        recordGpuPreflightError(this._canvasId, 0x0500);
        return null;
    }

    // The default framebuffer is the drawing buffer: BACK (colour), DEPTH and STENCIL, present as the context attributes
    // asked (WebGL 1.0 6.? / ES 3.0 6.1.13).
    _defaultFramebufferAttachmentParameter(attachment, pname) {
        if (attachment !== 0x0405 && attachment !== 0x1801 && attachment !== 0x1802) { recordGpuPreflightError(this._canvasId, 0x0500); return null; }
        const attributes = this.getContextAttributes() || {};
        const exists = attachment === 0x0405 ? true : (attachment === 0x1801 ? attributes.depth !== false : attributes.stencil === true);
        switch (pname) {
            case 0x8cd0: return exists ? 0x8218 : 0;               // OBJECT_TYPE: FRAMEBUFFER_DEFAULT or NONE
            case 0x8212: case 0x8213: case 0x8214:                 // RED/GREEN/BLUE_SIZE
                return attachment === 0x0405 ? 8 : 0;
            case 0x8215: return attachment === 0x0405 && attributes.alpha !== false ? 8 : 0;   // ALPHA_SIZE
            case 0x8216: return attachment === 0x1801 && exists ? 24 : 0;                       // DEPTH_SIZE
            case 0x8217: return attachment === 0x1802 && exists ? 8 : 0;                        // STENCIL_SIZE
            case 0x8211: return exists ? 0x8c17 : 0;                                              // COMPONENT_TYPE: UNSIGNED_NORMALIZED
            case 0x8210: return exists ? 0x2601 : 0;                                              // COLOR_ENCODING: LINEAR
            default: recordGpuPreflightError(this._canvasId, 0x0500); return null;       // an object name, level or face of the default framebuffer: INVALID_ENUM
        }
    }
    // FRAMEBUFFER -- and WebGL 2's READ_FRAMEBUFFER and DRAW_FRAMEBUFFER -- only: another target is INVALID_ENUM and 0.
    // A framebuffer the facade finds incomplete (`framebufferStatus`) is answered here; one it finds complete is asked
    // of the driver once a configuration, since an implementation may refuse a combination of formats the rules allow
    // (FRAMEBUFFER_UNSUPPORTED), and every call that draws or reads is judged by that answer from then on.
    checkFramebufferStatus(target) {
        const t = Number(target) >>> 0;
        if (t !== 0x8d40 && !((t === 0x8ca8 || t === 0x8ca9) && this._isWebGL2())) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return 0;
        }
        const fb = t === 0x8ca8 ? this._readFramebufferBinding : this._framebufferBinding;
        const status = framebufferStatus(this, fb);
        if (status !== GL_FRAMEBUFFER_COMPLETE || fb === null) return status;
        if (fb._driverGeneration !== fb._statusGeneration) {
            fb._driverGeneration = fb._statusGeneration;
            const driver = _rawCheckFramebufferStatus(this._canvasId, t);
            if (driver !== 0 && driver !== GL_FRAMEBUFFER_COMPLETE) fb._status = driver;
        }
        return fb._status;
    }
    createRenderbuffer() {
        const id = nextResourceId();
        _rawCreateRenderbuffer(this._canvasId, id);
        return new WebglObject(id, "renderbuffer", this._canvasId);
    }
    // A deleted renderbuffer leaves RENDERBUFFER and the framebuffers bound (ES 3.0 4.4.2.1, D.1.2).
    deleteRenderbuffer(rb) {
        if (!this._deletes("deleteRenderbuffer", rb, "renderbuffer", "WebGLRenderbuffer")) return;
        rb._deleted = true;
        if (this._renderbufferBinding === rb) this._renderbufferBinding = null;
        this._detachFromBoundFramebuffers(rb);
        _rawDeleteRenderbuffer(rb._id);
    }
    bindRenderbuffer(target, rb) {
        if ((Number(target) >>> 0) !== 0x8d41) {            // RENDERBUFFER
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return;
        }
        const bound = rb === undefined ? null : rb;
        const error = this._bindError("bindRenderbuffer", bound, "renderbuffer", "WebGLRenderbuffer");
        if (error !== 0) {
            recordGpuPreflightError(this._canvasId, error);
            return;
        }
        if (bound !== null) bound._everBound = true;
        this._renderbufferBinding = bound;
        const rbId = bound ? bound._id : -1;
        // opcode 13: H C U I.
        if (typeof target === "number" && typeof rbId === "number") {
            encodeBindRenderbuffer(this._canvasId, target >>> 0, rbId | 0);
            return;
        }
        flushRenderCommandStream();
        _rawBindRenderbuffer(this._canvasId, target, rbId);
    }
    // Refused before anything is sent: a target other than RENDERBUFFER is INVALID_ENUM, no renderbuffer bound
    // INVALID_OPERATION, an internal format the context has no renderbuffer of INVALID_ENUM (`_WEBGL1_RENDERBUFFER_FORMATS`,
    // `_WEBGL2_RENDERBUFFER_FORMATS`), a size or sample count out of range INVALID_VALUE. 0 when none is.
    _renderbufferStorageError(target, internalformat, width, height, samples) {
        if ((Number(target) >>> 0) !== 0x8d41) return GL_INVALID_ENUM;
        if (this._renderbufferBinding === null) return GL_INVALID_OPERATION;
        const i = Number(internalformat) >>> 0;
        const webgl2 = this._isWebGL2();
        if (!_listHas(webgl2 ? _WEBGL2_RENDERBUFFER_FORMATS : _WEBGL1_RENDERBUFFER_FORMATS, i)) {
            // A float format while it is colour-renderable; WebGL 1 names two, RGBA32F_EXT and RGBA16F_EXT, and has
            // EXT_sRGB's SRGB8_ALPHA8_EXT. RGB16F_EXT is not offered: no framebuffer here renders to it.
            const info = _FORMAT_INFO.get(i);
            const offered = !webgl2 && i === 0x8c43 ? this._extSrgb !== undefined
                : (webgl2 || i === 0x8814 || i === 0x881a) && info !== undefined && floatColourRenderable(this, info);
            if (!offered) return GL_INVALID_ENUM;
        }
        if (!NumberIsInteger(width) || width < 0 || width > MAX_WEBGL_GPU_2D_DIMENSION ||
                !NumberIsInteger(height) || height < 0 || height > MAX_WEBGL_GPU_2D_DIMENSION ||
                !NumberIsInteger(samples) || samples < 0 || samples > MAX_WEBGL_GPU_SAMPLES) {
            return GL_INVALID_VALUE;
        }
        return 0;
    }
    renderbufferStorage(target, internalformat, width, height) {
        const error = this._renderbufferStorageError(target, internalformat, width, height, 0);
        if (error !== 0) {
            recordGpuPreflightError(this._canvasId, error);
            return;
        }
        this._noteRenderbufferStorage(internalformat, width, height);
        _rawRenderbufferStorage(this._canvasId, target, internalformat, width, height);
    }

    // What getRenderbufferParameter answers, recorded on the renderbuffer bound to RENDERBUFFER.
    _noteRenderbufferStorage(internalformat, width, height, samples = 0) {
        const rb = this._renderbufferBinding;
        if (!rb || typeof internalformat !== "number" || typeof width !== "number" || typeof height !== "number") return;
        rb._format = internalformat >>> 0;
        rb._width = width >>> 0;
        rb._height = height >>> 0;
        rb._samples = samples >>> 0;
        framebufferChanged();
    }

    getRenderbufferParameter(target, pname) {
        if (target !== 0x8d41) { recordGpuPreflightError(this._canvasId, 0x0500); return null; }   // RENDERBUFFER only
        const rb = this._renderbufferBinding;
        if (!rb) { recordGpuPreflightError(this._canvasId, 0x0502); return null; }
        const format = rb._format === undefined ? 0x8056 : rb._format;       // RGBA4 until storage is given
        switch (pname >>> 0) {
            case 0x8d42: return rb._width || 0;       // RENDERBUFFER_WIDTH
            case 0x8d43: return rb._height || 0;      // RENDERBUFFER_HEIGHT
            case 0x8d44: return format;               // RENDERBUFFER_INTERNAL_FORMAT
            case 0x8d50: return _renderbufferBits(format, 0); // RED_SIZE
            case 0x8d51: return _renderbufferBits(format, 1); // GREEN_SIZE
            case 0x8d52: return _renderbufferBits(format, 2); // BLUE_SIZE
            case 0x8d53: return _renderbufferBits(format, 3); // ALPHA_SIZE
            case 0x8d54: return _renderbufferBits(format, 4); // DEPTH_SIZE
            case 0x8d55: return _renderbufferBits(format, 5); // STENCIL_SIZE
            default: break;
        }
        recordGpuPreflightError(this._canvasId, 0x0500);
        return null;
    }

    // -- Phase 3B: Misc --

    // WebGL 1's formats and types first (`_readPixelsEnumError`), then a view that is not of the type, then the read
    // buffer (`_refusesRead`), in a browser's order; which pairs the read buffer can be read as is the driver's to say.
    // HALF_FLOAT_OES is ES 3.0's HALF_FLOAT to the driver.
    readPixels(x, y, width, height, format, type, pixels) {
        checkReadPixelsDestination(pixels, true);
        const t = Number(type) >>> 0;
        const error = this._readPixelsEnumError(format, t) || (pixels == null ? 0 : _readViewError(pixels, t));
        if (error !== 0) {
            recordGpuPreflightError(this._canvasId, error);
            return;
        }
        if (this._refusesRead()) return;
        readPixelsIntoView(this._canvasId, x, y, width, height, format, t === 0x8d61 ? _HALF : type, pixels, 0);
    }
    // WebGL 1's read formats ALPHA, RGB and RGBA; its types UNSIGNED_BYTE and the three packed shorts, FLOAT with
    // OES_texture_float or OES_texture_half_float and HALF_FLOAT_OES with the latter; and what a driver's
    // IMPLEMENTATION_COLOR_READ pair may be besides (EXT_read_format_bgra's BGRA_EXT and its two reversed shorts).
    // INVALID_ENUM for anything else.
    _readPixelsEnumError(format, type) {
        const f = Number(format) >>> 0;
        if (f !== 0x1906 && f !== 0x1907 && f !== 0x1908 && f !== 0x80e1) return GL_INVALID_ENUM;
        switch (type) {
            case _UBYTE: case 0x8363: case 0x8033: case 0x8034: case 0x8365: case 0x8366: return 0;
            case _FLOAT:
                return this._oesTextureFloat !== undefined || this._oesTextureHalfFloat !== undefined ? 0 : GL_INVALID_ENUM;
            case 0x8d61: return this._oesTextureHalfFloat !== undefined ? 0 : GL_INVALID_ENUM;
            default: return GL_INVALID_ENUM;
        }
    }
    // A read from a read framebuffer that is not complete is INVALID_FRAMEBUFFER_OPERATION, one whose read buffer is
    // NONE or names no image INVALID_OPERATION. True when refused, the error recorded.
    _refusesRead() {
        if (refusesIncompleteFramebuffer(this, this._readFramebufferBinding)) return true;
        if (readColorFormat(this) !== 0) return false;
        recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
        return true;
    }
    // GENERATE_MIPMAP_HINT, and FRAGMENT_SHADER_DERIVATIVE_HINT in WebGL 2 or with OES_standard_derivatives enabled;
    // FASTEST, NICEST or DONT_CARE. Anything else is INVALID_ENUM, before anything is sent.
    hint(target, mode) {
        const t = Number(target) >>> 0, m = Number(mode) >>> 0;
        if (!(t === 0x8192 || (t === 0x8b8b && (this._isWebGL2() || this._oesStandardDerivatives !== undefined))) ||
                (m !== 0x1100 && m !== 0x1101 && m !== 0x1102)) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return;
        }
        // opcode 44: H C U U.
        if (typeof target === "number" && typeof mode === "number") {
            encodeHint(this._canvasId, target >>> 0, mode >>> 0);
            return;
        }
        flushRenderCommandStream();
        _rawHint(this._canvasId, target, mode);
    }
}

Object.assign(WebGLRenderingContext.prototype, WebglConstants);

/**
 * WebGL 2.0 facade.  Extends `WebGLRenderingContext` with the ES 3.0
 * additions backed by the handler in
 * engine/crates/graphics/renderergl/handler.rs and the op wrappers in
 * engine/crates/runtime-v8/rendering/webgl/webgl.rs.
 *
 * Minimum footprint: VAO, instancing, UBO, sampler objects, sync
 * objects, immutable texture storage, BlitFramebuffer,
 * InvalidateFramebuffer, MSAA renderbuffers, and multiple draw/read
 * buffers.  More advanced features (Transform Feedback, Query Objects,
 * texImage3D, compressedTexSubImage3D, ...) will be added on demand as
 * real-world games request them -- they all share the same GLCmd + op
 * + handler layer as the items above.
 */
class WebGL2RenderingContext extends WebGLRenderingContext {
    constructor(canvas, options) {
        super(canvas, options);
        this._webgl2 = true;
        this._currentQueryByTarget = new Map();     // query slot (`_querySlot`) -> the active query
        this._uniformBufferBindings = new Map();
        // The default transform feedback object's state; one the content made keeps its own (`createTransformFeedback`).
        this._defaultTransformFeedback = { bindings: new Map(), genericBuffer: null, active: false, paused: false };
        this._samplerBindings = new Map();          // texture unit index -> the sampler bound to it
        this._textureBindings3D = new Map();        // texture unit -> WebglObject|null
        this._textureBindings2DArray = new Map();
        this._copyReadBufferBinding = null;
        this._copyWriteBufferBinding = null;
        this._pixelPackBufferBinding = null;
        this._pixelUnpackBufferBinding = null;
        this._uniformBufferBinding = null;       // the generic UNIFORM_BUFFER binding
        this._maxUniformBufferBindings = 0;
        this._maxTransformFeedbackBindings = 0;
        this._uniformBufferOffsetAlignment = 0;
        // `clearBuffer*` fills it with the four values of the record it encodes.
        this._clearBufferScratch = [0, 0, 0, 0];
        this._attribMinimum = 16;     // WebGL 2's MAX_VERTEX_ATTRIBS is at least this
        this._textureUnitMinimum = 32;
        for (const pname of [0x0d02, 0x0d04, 0x0d03, 0x0cf2, 0x806e, 0x0cf4, 0x0cf3, 0x806d]) this._pixelStore.set(pname, 0);
        this._maxDrawBuffers = 0;
        this._maxColorAttachments = 0;
        // Scratch for `uniform{1,2,3,4}ui`: the stream copies the words as it encodes them.
        this._uniformU32Scratch = [null, new Uint32Array(1), new Uint32Array(2), new Uint32Array(3), new Uint32Array(4)];
        this._currentTransformFeedback = null;
    }

    _textureBindings(target) {
        if (target === 0x806f) return this._textureBindings3D;        // TEXTURE_3D
        if (target === 0x8c1a) return this._textureBindings2DArray;   // TEXTURE_2D_ARRAY
        return super._textureBindings(target);
    }

    // WebGL 2's buffer targets. The generic TRANSFORM_FEEDBACK_BUFFER binding is the bound transform feedback object's,
    // as its indexed bindings are (ES 3.0 2.15.1, table 6.24).
    _boundBuffer(target) {
        switch (target) {
            case 0x8f36: return this._copyReadBufferBinding;
            case 0x8f37: return this._copyWriteBufferBinding;
            case 0x88eb: return this._pixelPackBufferBinding;
            case 0x88ec: return this._pixelUnpackBufferBinding;
            case 0x8a11: return this._uniformBufferBinding;
            case 0x8c8e: return this._transformFeedbackState().genericBuffer;
            default: return super._boundBuffer(target);
        }
    }
    _setBoundBuffer(target, buffer) {
        switch (target) {
            case 0x8f36: this._copyReadBufferBinding = buffer; break;
            case 0x8f37: this._copyWriteBufferBinding = buffer; break;
            case 0x88eb: this._pixelPackBufferBinding = buffer; break;
            case 0x88ec: this._pixelUnpackBufferBinding = buffer; break;
            case 0x8a11: this._uniformBufferBinding = buffer; break;
            case 0x8c8e: this._transformFeedbackState().genericBuffer = buffer; break;
            default: super._setBoundBuffer(target, buffer);
        }
    }
    _unbindDeletedBuffer(buffer) {
        super._unbindDeletedBuffer(buffer);
        for (const target of [0x8f36, 0x8f37, 0x88eb, 0x88ec, 0x8a11, 0x8c8e]) {
            if (this._boundBuffer(target) === buffer) this._setBoundBuffer(target, null);
        }
        for (const bindings of [this._uniformBufferBindings, this._transformFeedbackState().bindings]) {
            for (const [index, binding] of bindings) if (binding.buffer === buffer) bindings.delete(index);
        }
    }
    // The bound transform feedback object's buffer state: its indexed bindings and its generic binding.
    _transformFeedbackState() {
        return this._currentTransformFeedback || this._defaultTransformFeedback;
    }

    readPixels(x, y, width, height, format, type, pixels, dstOffset = 0) {
        // Overload resolution by WebIDL: a number selects the PIXEL_PACK_BUFFER
        // form, where the seventh argument is a GLintptr byte offset into the
        // bound buffer and there is no eighth. Anything else must be a view,
        // and `dstData` is not nullable in this version.
        if (typeof pixels === "number") {
            if (this._refusesRead()) return;
            // GLintptr is a long long: truncate toward zero without wrapping,
            // and let the native side reject a negative offset.
            _rawReadPixelsToBuffer(this._canvasId, x, y, width, height, format, type,
                MathTrunc(pixels));
            return;
        }
        checkReadPixelsDestination(pixels, false);
        if (this._refusesRead()) return;
        // ToNumber runs once, before native view metadata is inspected. Unary
        // plus preserves WebIDL's TypeError for BigInt and Symbol inputs.
        readPixelsIntoView(this._canvasId, x, y, width, height, format, type, pixels, +dstOffset);
    }

    // ---- Vertex Array Objects ----------------------------------
    createVertexArray() { return this._createVertexArray(); }
    deleteVertexArray(vao) { this._deleteVertexArray("deleteVertexArray", vao); }
    isVertexArray(vao) { return this._isVertexArray(vao); }
    bindVertexArray(vao) { this._bindVertexArray("bindVertexArray", vao); }

    // ---- Integer vertex attributes (WebGL 2) ------------------------------------------------------------------------
    vertexAttribI4i(index, x, y, z, w) {
        if (typeof index === "number" && typeof x === "number" && typeof y === "number" &&
            typeof z === "number" && typeof w === "number") {
            encodeVertexAttribI4i(this._canvasId, index, x, y, z, w);
        } else {
            flushRenderCommandStream();
            _rawVertexAttribI4i(this._canvasId, index, x, y, z, w);
        }
        const i = Number(index) >>> 0;
        if (i < _ATTRIB_SHADOW_SLOTS) {
            const v = this._currentAttribI, k = i * 4;
            v[k] = Number(x); v[k + 1] = Number(y); v[k + 2] = Number(z); v[k + 3] = Number(w);
            this._currentAttribKind[i] = 1;
        }
    }
    vertexAttribI4ui(index, x, y, z, w) {
        if (typeof index === "number" && typeof x === "number" && typeof y === "number" &&
            typeof z === "number" && typeof w === "number") {
            encodeVertexAttribI4ui(this._canvasId, index, x, y, z, w);
        } else {
            flushRenderCommandStream();
            _rawVertexAttribI4ui(this._canvasId, index, x, y, z, w);
        }
        const i = Number(index) >>> 0;
        if (i < _ATTRIB_SHADOW_SLOTS) {
            const v = this._currentAttribU, k = i * 4;
            v[k] = Number(x); v[k + 1] = Number(y); v[k + 2] = Number(z); v[k + 3] = Number(w);
            this._currentAttribKind[i] = 2;
        }
    }
    vertexAttribI4iv(index, v) { if (this._attribList("vertexAttribI4iv", v, 4)) this.vertexAttribI4i(index, v[0], v[1], v[2], v[3]); }
    vertexAttribI4uiv(index, v) { if (this._attribList("vertexAttribI4uiv", v, 4)) this.vertexAttribI4ui(index, v[0], v[1], v[2], v[3]); }
    vertexAttribIPointer(index, size, type, stride, offset) {
        if (typeof index === "number" && typeof size === "number" && typeof type === "number" &&
            typeof stride === "number" && typeof offset === "number") {
            encodeVertexAttribIPointer(this._canvasId, index >>> 0, size | 0, type >>> 0, stride | 0, offset | 0);
        } else {
            flushRenderCommandStream();
            _rawVertexAttribIPointer(this._canvasId, index, size, type, stride, offset);
        }
        this._shadowAttribPointer(index, size, type, false, true, stride, offset);
    }

    // ---- clearBuffer* and drawRangeElements (WebGL 2) ---------------------------------------------------------------
    // The draw-buffer-specific clears. What the buffer enum may be depends on the type of the values (fv: COLOR or DEPTH,
    // iv: COLOR or STENCIL, uiv: COLOR, fi: DEPTH_STENCIL); the draw buffer is below MAX_DRAW_BUFFERS for COLOR and 0 for
    // the rest; a list holds the elements the buffer needs (4 for COLOR, 1 otherwise) from `srcOffset`. The arguments are
    // converted first, as WebIDL converts them before the call runs (a value that is not a list is a TypeError ahead of any
    // GL error); then a call that breaks a rule is the error the specification names, in that order, and sends nothing.
    _drawBufferLimit() { return this._cachedLimit("_maxDrawBuffers", 0x8824, 4); }            // MAX_DRAW_BUFFERS
    // `other` is the buffer besides COLOR the call takes: DEPTH for fv, STENCIL for iv, none (-1) for uiv.
    _clearBufferValues(name, other, Type, buffer, drawbuffer, values, srcOffset) {
        const b = buffer >>> 0;
        const d = drawbuffer | 0;
        let list = values;
        if (isTypedArray(list)) {
            if (!(list instanceof Type)) list = new Type(list);
        } else if (ArrayIsArray(list)) {
            list = new Type(list);
        } else {
            throw new TypeError(`Failed to execute '${name}' on 'WebGL2RenderingContext': parameter 3 is not of type '${Type.name.slice(0, -5)}List'.`);
        }
        const offset = toUnsignedLongLong(srcOffset);
        if (b !== 0x1800 && b !== other) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return null;
        }
        const color = b === 0x1800;
        if (d < 0 || (color ? d >= 4 && d >= this._drawBufferLimit() : d !== 0)) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
            return null;
        }
        if (offset > list.length || list.length - offset < (color ? 4 : 1)) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
            return null;
        }
        // The components a buffer does not use are 0, so the record has one shape.
        const v = this._clearBufferScratch;
        v[0] = list[offset];
        v[1] = color ? list[offset + 1] : 0;
        v[2] = color ? list[offset + 2] : 0;
        v[3] = color ? list[offset + 3] : 0;
        return v;
    }
    // Each after its own checks (`_clearBufferValues`), a draw framebuffer that is not complete being
    // INVALID_FRAMEBUFFER_OPERATION.
    clearBufferfv(buffer, drawbuffer, values, srcOffset = 0) {
        const v = this._clearBufferValues("clearBufferfv", 0x1801, Float32Array, buffer, drawbuffer, values, srcOffset);
        if (v === null || refusesIncompleteFramebuffer(this, this._framebufferBinding)) return;
        encodeClearBufferfv(this._canvasId, buffer, drawbuffer, v[0], v[1], v[2], v[3]);
    }
    clearBufferiv(buffer, drawbuffer, values, srcOffset = 0) {
        const v = this._clearBufferValues("clearBufferiv", 0x1802, Int32Array, buffer, drawbuffer, values, srcOffset);
        if (v === null || refusesIncompleteFramebuffer(this, this._framebufferBinding)) return;
        encodeClearBufferiv(this._canvasId, buffer, drawbuffer, v[0], v[1], v[2], v[3]);
    }
    clearBufferuiv(buffer, drawbuffer, values, srcOffset = 0) {
        const v = this._clearBufferValues("clearBufferuiv", -1, Uint32Array, buffer, drawbuffer, values, srcOffset);
        if (v === null || refusesIncompleteFramebuffer(this, this._framebufferBinding)) return;
        encodeClearBufferuiv(this._canvasId, buffer, drawbuffer, v[0], v[1], v[2], v[3]);
    }
    clearBufferfi(buffer, drawbuffer, depth, stencil) {
        const b = buffer >>> 0;
        const d = drawbuffer | 0;
        const z = +depth;                        // GLfloat: the record rounds it to a float32
        const s = stencil | 0;
        if (b !== 0x84F9) {                      // DEPTH_STENCIL
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
        } else if (d !== 0) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
        } else {
            encodeClearBufferfi(this._canvasId, b, d, z, s);
        }
    }
    // `start` and `end` are the range of indices the call may read, a hint the driver may use; the call draws what
    // `drawElements` would. An `end` below `start` is INVALID_VALUE.
    drawRangeElements(mode, start, end, count, type, offset) {
        if ((end >>> 0) < (start >>> 0)) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
            return;
        }
        this.drawElements(mode, count, type, offset);
    }

    // ---- Unsigned integer uniforms and the non-square matrices (WebGL 2) --------------------------------------------
    // `uniform{1,2,3,4}ui` are the component form of the `uiv` record of the same width, as `uniform2i` is of `uniform2iv`.
    _uniformUi(n, location, a, b, c, d) {
        const loc = _loc(location);
        const payload = this._uniformU32Scratch[n];
        payload[0] = a >>> 0;
        if (n > 1) payload[1] = b >>> 0;
        if (n > 2) payload[2] = c >>> 0;
        if (n > 3) payload[3] = d >>> 0;
        if (!_UNIFORM_UI_ENCODERS[n](this._canvasId, loc, payload)) {
            flushRenderCommandStream();
            _UNIFORM_UI_RAW[n](this._canvasId, loc, payload);
        }
    }
    uniform1ui(location, v0) { this._uniformUi(1, location, v0); }
    uniform2ui(location, v0, v1) { this._uniformUi(2, location, v0, v1); }
    uniform3ui(location, v0, v1, v2) { this._uniformUi(3, location, v0, v1, v2); }
    uniform4ui(location, v0, v1, v2, v3) { this._uniformUi(4, location, v0, v1, v2, v3); }
    _uniformUiv(n, name, location, data, srcOffset, srcLength) {
        const loc = _loc(location);
        const payload = _uniformListPayload(this._canvasId, name, data, srcOffset, srcLength, n, Uint32Array);
        if (payload === null) return;
        if (!_UNIFORM_UI_ENCODERS[n](this._canvasId, loc, payload)) {
            flushRenderCommandStream();
            _UNIFORM_UI_RAW[n](this._canvasId, loc, payload);
        }
    }
    uniform1uiv(location, data, srcOffset = 0, srcLength = 0) { this._uniformUiv(1, "uniform1uiv", location, data, srcOffset, srcLength); }
    uniform2uiv(location, data, srcOffset = 0, srcLength = 0) { this._uniformUiv(2, "uniform2uiv", location, data, srcOffset, srcLength); }
    uniform3uiv(location, data, srcOffset = 0, srcLength = 0) { this._uniformUiv(3, "uniform3uiv", location, data, srcOffset, srcLength); }
    uniform4uiv(location, data, srcOffset = 0, srcLength = 0) { this._uniformUiv(4, "uniform4uiv", location, data, srcOffset, srcLength); }
    _uniformMatrixNxM(name, unit, encode, raw, location, transpose, data, srcOffset, srcLength) {
        const loc = _loc(location);
        const payload = _uniformListPayload(this._canvasId, name, data, srcOffset, srcLength, unit, Float32Array);
        if (payload === null) return;
        const t = !!transpose;
        if (!encode(this._canvasId, loc, t, payload)) {
            flushRenderCommandStream();
            raw(this._canvasId, loc, t, payload);
        }
    }
    uniformMatrix2x3fv(location, transpose, data, srcOffset = 0, srcLength = 0) {
        this._uniformMatrixNxM("uniformMatrix2x3fv", 6, encodeUniformMatrix2x3fv, _rawUniformMatrix2x3fv, location, transpose, data, srcOffset, srcLength);
    }
    uniformMatrix2x4fv(location, transpose, data, srcOffset = 0, srcLength = 0) {
        this._uniformMatrixNxM("uniformMatrix2x4fv", 8, encodeUniformMatrix2x4fv, _rawUniformMatrix2x4fv, location, transpose, data, srcOffset, srcLength);
    }
    uniformMatrix3x2fv(location, transpose, data, srcOffset = 0, srcLength = 0) {
        this._uniformMatrixNxM("uniformMatrix3x2fv", 6, encodeUniformMatrix3x2fv, _rawUniformMatrix3x2fv, location, transpose, data, srcOffset, srcLength);
    }
    uniformMatrix3x4fv(location, transpose, data, srcOffset = 0, srcLength = 0) {
        this._uniformMatrixNxM("uniformMatrix3x4fv", 12, encodeUniformMatrix3x4fv, _rawUniformMatrix3x4fv, location, transpose, data, srcOffset, srcLength);
    }
    uniformMatrix4x2fv(location, transpose, data, srcOffset = 0, srcLength = 0) {
        this._uniformMatrixNxM("uniformMatrix4x2fv", 8, encodeUniformMatrix4x2fv, _rawUniformMatrix4x2fv, location, transpose, data, srcOffset, srcLength);
    }
    uniformMatrix4x3fv(location, transpose, data, srcOffset = 0, srcLength = 0) {
        this._uniformMatrixNxM("uniformMatrix4x3fv", 12, encodeUniformMatrix4x3fv, _rawUniformMatrix4x3fv, location, transpose, data, srcOffset, srcLength);
    }

    // ---- Instanced drawing -------------------------------------
    // An index past MAX_VERTEX_ATTRIBS is INVALID_VALUE.
    vertexAttribDivisor(index, divisor) {
        if (!this._isAttribIndex(Number(index) >>> 0)) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
            return;
        }
        // opcode 19: H C U U.
        if (typeof index === "number" && typeof divisor === "number") {
            encodeVertexAttribDivisor(this._canvasId, index >>> 0, divisor >>> 0);
        } else {
            flushRenderCommandStream();
            _rawVertexAttribDivisor(this._canvasId, index, divisor);
        }
        const i = Number(index) >>> 0;
        if (i < _ATTRIB_SHADOW_SLOTS) this._attribShadow.divisor[i] = Number(divisor) >>> 0;
    }
    drawArraysInstanced(mode, first, count, instanceCount) {
        const error = this._drawError(mode, Number(first) | 0, Number(count) | 0, Number(instanceCount) | 0, undefined, 0);
        if (error !== 0) {
            recordGpuPreflightError(this._canvasId, error);
            return;
        }
        // opcode 49: H C U I I I.
        if (typeof mode === "number" && typeof first === "number" &&
            typeof count === "number" && typeof instanceCount === "number") {
            encodeDrawArraysInstanced(this._canvasId, mode >>> 0, first | 0, count | 0, instanceCount | 0);
            return;
        }
        flushRenderCommandStream();
        _rawDrawArraysInstanced(this._canvasId, mode, first, count, instanceCount);
    }
    drawElementsInstanced(mode, count, type, offset, instanceCount) {
        const error = this._drawError(mode, 0, Number(count) | 0, Number(instanceCount) | 0, type, toLongLong(Number(offset)));
        if (error !== 0) {
            recordGpuPreflightError(this._canvasId, error);
            return;
        }
        // opcode 50: H C U I U I I.
        if (typeof mode === "number" && typeof count === "number" &&
            typeof type === "number" && typeof offset === "number" &&
            typeof instanceCount === "number") {
            encodeDrawElementsInstanced(this._canvasId, mode >>> 0, count | 0, type >>> 0, offset | 0, instanceCount | 0);
            return;
        }
        flushRenderCommandStream();
        _rawDrawElementsInstanced(this._canvasId, mode, count, type, offset, instanceCount);
    }

    // ---- Renderbuffer formats ----------------------------------
    // `getInternalformatParameter(RENDERBUFFER, format, SAMPLES)`: the sample counts the driver supports for a
    // renderbuffer of `format`, sorted descending, an empty Int32Array when it cannot be multisampled. WebGL 2 allows
    // nothing else: any other target or pname is INVALID_ENUM and answers null. Pixi and three.js ask this when
    // they build a multisampled render target, and Pixi asks it while it creates its renderer.
    getInternalformatParameter(target, internalformat, pname) {
        if (target !== WebglConstants.RENDERBUFFER || pname !== WebglConstants.SAMPLES) {
            recordGpuPreflightError(this._canvasId, WebglConstants.INVALID_ENUM);
            return null;
        }
        const json = _rawGetGlState(this._canvasId, GL_STATE_INTERNALFORMAT_SAMPLES, target, internalformat >>> 0, "");
        let samples = [];
        try { samples = JSON.parse(json); } catch (_) { /* no answer: no counts */ }
        return new Int32Array(Array.isArray(samples) ? samples : []);
    }

    // ---- Uniform Buffer Objects --------------------------------
    getUniformBlockIndex(program, name) {
        const programId = program?.id;
        // The same sentinel a lookup that finds no block returns, so one check
        // covers both. `-1` would be a third answer this API never otherwise
        // produces, and a GLuint return type cannot carry it anyway.
        if (programId === undefined) return WebglConstants.INVALID_INDEX;
        let inner = this._uniformBlockIndexCache.get(programId);
        let index = inner && inner.get(name);
        if (index === undefined) {
            index = _rawGetUniformBlockIndex(programId, name);
            if (!inner) {
                inner = new Map();
                this._uniformBlockIndexCache.set(programId, inner);
            }
            inner.set(name, index);
        }
        return index;
    }

    // The location of a fragment shader output (`layout(location = n) out`), -1 for a name that is not one. Names are
    // checked as `getAttribLocation` checks them.
    getFragDataLocation(program, name) {
        if (!(program instanceof WebglObject) || program._kind !== "program") {
            throw new TypeError("Failed to execute 'getFragDataLocation' on 'WebGL2RenderingContext': parameter 1 is not of type 'WebGLProgram'.");
        }
        const key = `${name}`;
        if (!this._isLive(program, "program")) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return -1;
        }
        const error = _glslNameError(key, this._maxNameLength());
        if (error !== 0) {
            recordGpuPreflightError(this._canvasId, error);
            return -1;
        }
        if (_isReservedGlslName(key)) return -1;
        const location = this._programState("getFragDataLocation", program, GL_STATE_FRAG_DATA_LOCATION, 0, key);
        return location === undefined ? -1 : location;
    }

    getActiveUniformBlockName(program, uniformBlockIndex) {
        const name = this._programState("getActiveUniformBlockName", program, GL_STATE_ACTIVE_UNIFORM_BLOCK_NAME, uniformBlockIndex >>> 0, "");
        return typeof name === "string" ? name : null;
    }

    getActiveUniformBlockParameter(program, uniformBlockIndex, pname) {
        const value = this._programState("getActiveUniformBlockParameter", program, GL_STATE_ACTIVE_UNIFORM_BLOCK_PARAMETER, uniformBlockIndex >>> 0, String(pname >>> 0));
        if (value === undefined) return null;
        return Array.isArray(value) ? new Uint32Array(value) : value;
    }

    // The wire carries a name of at most 1024 bytes, so a long list goes in pieces. A GLSL identifier is ASCII and
    // WebGL caps it at 256 characters: a name with a newline (the separator) or past that cannot be one, and is
    // INVALID_INDEX without asking.
    getUniformIndices(program, uniformNames) {
        const names = Array.from(uniformNames, String);
        const found = new Array(names.length).fill(WebglConstants.INVALID_INDEX);
        let batch = [];
        let batchChars = 0;
        const flush = () => {
            if (batch.length === 0) return true;
            const indices = this._programState("getUniformIndices", program, GL_STATE_UNIFORM_INDICES, 0, batch.map((entry) => entry.name).join("\n"));
            if (!Array.isArray(indices)) return false;
            for (let i = 0; i < batch.length; i++) found[batch[i].at] = indices[i];
            batch = [];
            batchChars = 0;
            return true;
        };
        for (let at = 0; at < names.length; at++) {
            const name = names[at];
            if (name.length > 256 || name.includes("\n")) continue;
            if (batchChars + name.length + 1 > 300 && !flush()) return null;
            batch.push({ at, name });
            batchChars += name.length + 1;
        }
        if (batch.length === 0) {
            // Nothing to ask, but a program that is not linked is still an error: ask about no name in particular.
            if (this._programState("getUniformIndices", program, GL_STATE_UNIFORM_INDICES, 0, "") === undefined) return null;
        } else if (!flush()) {
            return null;
        }
        return found;
    }

    getActiveUniforms(program, uniformIndices, pname) {
        if (!(program instanceof WebglObject) || program._kind !== "program") {
            throw new TypeError("getActiveUniforms: parameter 1 is not of type 'WebGLProgram'.");
        }
        pname = pname >>> 0;
        // UNIFORM_NAME_LENGTH (0x8A39) is the one pname in that run that WebGL leaves out.
        if (pname < WebglConstants.UNIFORM_TYPE || pname > WebglConstants.UNIFORM_IS_ROW_MAJOR || pname === 0x8a39) {
            recordGpuPreflightError(this._canvasId, WebglConstants.INVALID_ENUM);
            return null;
        }
        const indices = Array.from(uniformIndices, (i) => i >>> 0);
        const result = [];
        // 80 indices of at most ten digits and a comma stay under the wire's 1024 bytes.
        for (let at = 0; at < indices.length; at += 80) {
            const values = this._programState("getActiveUniforms", program, GL_STATE_ACTIVE_UNIFORMS_PARAMETER, pname, indices.slice(at, at + 80).join(","));
            if (!Array.isArray(values)) return null;
            for (const value of values) result.push(value);
        }
        return result;
    }

    uniformBlockBinding(program, uniformBlockIndex, uniformBlockBinding) {
        _rawUniformBlockBinding(program._id, uniformBlockIndex, uniformBlockBinding);
    }
    // The record is made of the numbers each argument converts to; an argument that is not a Number takes the op
    // (see `_makeOrderedRaw`), as every call with a fast path does.
    // Binding an indexed point binds the generic one too (ES 3.0 2.10.1.1), and the buffer is checked as `bindBuffer`
    // checks it.
    bindBufferBase(target, index, buffer) {
        const t = Number(target) >>> 0;
        const i = Number(index) >>> 0;
        const bound = buffer === undefined ? null : buffer;
        const error = this._bufferBindError("bindBufferBase", 3, bound, t);
        if (error !== 0) {
            recordGpuPreflightError(this._canvasId, error);
            return;
        }
        if ((t === 0x8a11 || t === 0x8c8e) && this._indexedBindingLimit(t, i)) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
            return;
        }
        if (this._indexedBindTakes(t, bound, 0, 0, false)) {
            this._recordIndexedBind(t, i, bound, 0, 0);
            this._noteBufferBound(bound, t);
            this._setBoundBuffer(t, bound);
        }
        const bufferId = bound ? bound._id : 0;
        // opcode 51: H C U U U. bufferId 0 unbinds.
        if (typeof target === "number" && typeof index === "number") {
            encodeBindBufferBase(this._canvasId, t, i, bufferId >>> 0);
            return;
        }
        _rawBindBufferBase(this._canvasId, target, index, bufferId);
    }
    bindBufferRange(target, index, buffer, offset, size) {
        const t = Number(target) >>> 0;
        const i = Number(index) >>> 0;
        const bound = buffer === undefined ? null : buffer;
        const o = toLongLong(Number(offset));
        const n = toLongLong(Number(size));
        const error = this._bufferBindError("bindBufferRange", 3, bound, t);
        if (error !== 0) {
            recordGpuPreflightError(this._canvasId, error);
            return;
        }
        if ((t === 0x8a11 || t === 0x8c8e) && this._indexedBindingLimit(t, i)) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
            return;
        }
        // An offset or size past 2^31 cannot fit any buffer (the render side holds a buffer's size as a GLint).
        if (bound && (o > 0x7fffffff || n > 0x7fffffff)) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
            return;
        }
        if (bound && t === 0x8a11 && o > 0 &&
                o % this._cachedLimit("_uniformBufferOffsetAlignment", 0x8a34, 1) !== 0) {    // UNIFORM_BUFFER_OFFSET_ALIGNMENT
            recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
            return;
        }
        if (this._indexedBindTakes(t, bound, o, n, true)) {
            this._recordIndexedBind(t, i, bound, o, n);
            this._noteBufferBound(bound, t);
            this._setBoundBuffer(t, bound);
        }
        const bufferId = bound ? bound._id : 0;
        // opcode 52: H C U U U I I.
        if (typeof target === "number" && typeof index === "number" &&
                typeof offset === "number" && typeof size === "number") {
            encodeBindBufferRange(this._canvasId, t, i, bufferId >>> 0, o | 0, n | 0);
            return;
        }
        _rawBindBufferRange(this._canvasId, target, index, bufferId, offset, size);
    }

    // ---- Immutable texture storage ------------------------------
    // Refused before anything is sent: the internal format (`_storageFormatError`), the levels and size
    // (`preflightTexStorage2D`), and storage already immutable (`_refusesImmutable`). Every image it makes is recorded.
    texStorage2D(target, levels, internalformat, width, height) {
        const texture = this._textureFor(target, "storage2D");
        if (!texture) return;
        if (!preflightTexStorage2D(
            this._canvasId, target, levels, this._storageFormatError(internalformat, target), width, height,
        ) || this._refusesImmutable(texture)) return;
        _rawTexStorage2D(this._canvasId, target, levels, internalformat, width, height);
        defineTextureStorage(texture, target, levels, internalformat, width, height, 1);
        refreshTextureSampling(this, texture);
    }

    // ---- Framebuffer ops ---------------------------------------
    // A read or draw framebuffer that is not complete is INVALID_FRAMEBUFFER_OPERATION.
    blitFramebuffer(srcX0, srcY0, srcX1, srcY1, dstX0, dstY0, dstX1, dstY1, mask, filter) {
        if (refusesIncompleteFramebuffer(this, this._readFramebufferBinding) ||
                refusesIncompleteFramebuffer(this, this._framebufferBinding)) return;
        _rawBlitFramebuffer(this._canvasId, srcX0, srcY0, srcX1, srcY1,
                             dstX0, dstY0, dstX1, dstY1, mask, filter);
    }
    // The attachments `invalidateFramebuffer` / `invalidateSubFramebuffer` name, against the framebuffer the target has
    // bound: the default one names its buffers COLOR / DEPTH / STENCIL, an object its attachment points. A name that
    // framebuffer does not have is INVALID_ENUM, a colour attachment past MAX_COLOR_ATTACHMENTS INVALID_OPERATION, a
    // target that is not a framebuffer binding INVALID_ENUM. The list as the op takes it, or null when refused.
    _invalidationList(target, attachments) {
        const list = toGLenumSequence(attachments);
        const t = target >>> 0;
        if (t !== 0x8d40 && t !== 0x8ca9 && t !== 0x8ca8) {   // FRAMEBUFFER, DRAW_FRAMEBUFFER, READ_FRAMEBUFFER
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return null;
        }
        const fb = t === 0x8ca8 ? this._readFramebufferBinding : this._framebufferBinding;
        for (let k = 0; k < list.length; k++) {
            const a = list[k];
            let error = 0;
            if (!fb) {
                if (a < 0x1800 || a > 0x1802) error = GL_INVALID_ENUM;                             // COLOR, DEPTH, STENCIL
            } else if (a >= 0x8ce0 && a <= 0x8cef) {                                               // COLOR_ATTACHMENT0..15
                if (!this._hasColorAttachment(a - 0x8ce0)) error = GL_INVALID_OPERATION;
            } else if (a !== 0x8d00 && a !== 0x8d20 && a !== 0x821a) {                            // DEPTH, STENCIL, DEPTH_STENCIL
                error = GL_INVALID_ENUM;
            }
            if (error !== 0) {
                recordGpuPreflightError(this._canvasId, error);
                return null;
            }
        }
        return list;
    }
    invalidateFramebuffer(target, attachments) {
        const list = this._invalidationList(target, attachments);
        if (list !== null) _rawInvalidateFramebuffer(this._canvasId, target >>> 0, list);
    }
    invalidateSubFramebuffer(target, attachments, x, y, width, height) {
        const list = this._invalidationList(target, attachments);
        if (list === null) return;
        const w = width | 0;
        const h = height | 0;
        if (w < 0 || h < 0) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
            return;
        }
        _rawInvalidateSubFramebuffer(this._canvasId, target >>> 0, list, x | 0, y | 0, w, h);
    }
    // One layer of a 3D or 2D-array texture as an attachment. A target that is not a framebuffer binding is
    // INVALID_ENUM, a negative level or layer INVALID_VALUE. The layer is what FRAMEBUFFER_ATTACHMENT_TEXTURE_LAYER answers.
    // Refused as `framebufferTexture2D` is, a texture that is not a 3D or 2D-array one being INVALID_OPERATION, and a
    // level past its levels or a layer past MAX_3D_TEXTURE_SIZE / MAX_ARRAY_TEXTURE_LAYERS INVALID_VALUE.
    framebufferTextureLayer(target, attachment, texture, level, layer) {
        const t = Number(target) >>> 0;
        const object = texture === undefined ? null : texture;
        const lv = level | 0;
        const ly = layer | 0;
        const fb = this._attachmentFramebuffer(t, attachment, () => {
            if (object === null) return 0;
            if (!(object instanceof WebglObject) || object._kind !== "texture") {
                throw new TypeError("Failed to execute 'framebufferTextureLayer' on 'WebGL2RenderingContext': parameter 3 is not of type 'WebGLTexture'.");
            }
            if (object._deleted || object._ownerId !== this._canvasId ||
                    (object._target !== 0x806f && object._target !== 0x8c1a)) return GL_INVALID_OPERATION;
            const layers = object._target === 0x806f ? MAX_WEBGL_GPU_3D_DIMENSION : MAX_WEBGL_GPU_ARRAY_LAYERS;
            if (lv < 0 || lv >= this._levelLimit(object._target) || ly < 0 || ly >= layers) return GL_INVALID_VALUE;
            return 0;
        });
        if (fb === undefined) return;
        this._noteAttachment(t, attachment, object ? { type: 0x1702, object, level: lv, face: 0, layer: ly } : null);
        _rawFramebufferTextureLayer(this._canvasId, t, attachment >>> 0, object ? object.id : -1, lv, ly);
    }

    // ---- Copies (WebGL 2) -------------------------------------------------------------------------------------------
    copyTexSubImage3D(target, level, xoffset, yoffset, zoffset, x, y, width, height) {
        const texture = this._textureFor(target, "image3D");
        if (!texture || this._refusesCopyIntoImage(
            texture, target, level | 0, xoffset | 0, yoffset | 0, zoffset | 0, width | 0, height | 0,
        ) || this._refusesCopyFrom(this._image(texture, target, level | 0).internalformat)) return;
        encodeCopyTexSubImage3D(this._canvasId, target, level, xoffset, yoffset, zoffset, x, y, width, height);
    }
    // The offsets and size are `long long`, checked against the two buffers bound (ES 3.0 2.10.5, WebGL 2.0 5.1): a
    // target the context does not have is INVALID_ENUM; a negative offset or size, a range past either buffer and two
    // ranges of one buffer that overlap are INVALID_VALUE; no buffer bound, or an element-array buffer and one of
    // other data, INVALID_OPERATION. Every range then lies inside a buffer, whose size the render side holds as a
    // GLint, so the words that cross are exact.
    // `getBufferSubData(target, srcByteOffset, dstBuffer, dstOffset, length)`: `length` elements of `dstBuffer` from
    // `dstOffset` (the rest when 0) are filled with the bound buffer's bytes from `srcByteOffset`. A target the context
    // does not have is INVALID_ENUM; nothing bound, or TRANSFORM_FEEDBACK_BUFFER while transform feedback is active,
    // INVALID_OPERATION; a negative offset or a range past the view or the buffer INVALID_VALUE (WebGL 2.0 3.7.3). It
    // waits for the GPU work that writes the buffer, as the specification says it may.
    getBufferSubData(target, srcByteOffset, dstBuffer, dstOffset = 0, length = 0) {
        const t = Number(target) >>> 0;
        const offset = toLongLong(srcByteOffset);
        if (!ArrayBufferIsView(dstBuffer)) {
            throw new TypeError("Failed to execute 'getBufferSubData' on 'WebGL2RenderingContext': parameter 3 is not of type 'ArrayBufferView'.");
        }
        const bound = this._boundBuffer(t);
        let error = 0;
        if (bound === undefined) error = GL_INVALID_ENUM;
        else if (bound === null || (t === 0x8c8e && this._transformFeedbackState().active)) error = GL_INVALID_OPERATION;
        else if (offset < 0) error = GL_INVALID_VALUE;
        if (error !== 0) {
            recordGpuPreflightError(this._canvasId, error);
            return;
        }
        const destination = viewElementRange(this._canvasId, dstBuffer, dstOffset, length);
        if (destination === null) return;
        const bytes = TypedArrayPrototypeGetByteLength(destination);
        if (offset + bytes > (bound._size || 0)) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
            return;
        }
        _rawGetBufferSubData(this._canvasId, t, offset, destination);
    }

    copyBufferSubData(readTarget, writeTarget, readOffset, writeOffset, size) {
        const rt = Number(readTarget) >>> 0;
        const wt = Number(writeTarget) >>> 0;
        const r = toLongLong(readOffset);
        const w = toLongLong(writeOffset);
        const n = toLongLong(size);
        const read = this._boundBuffer(rt);
        const write = this._boundBuffer(wt);
        let error = 0;
        if (read === undefined || write === undefined) error = GL_INVALID_ENUM;
        else if (!(r >= 0 && w >= 0 && n >= 0)) error = GL_INVALID_VALUE;
        else if (read === null || write === null) error = GL_INVALID_OPERATION;
        else if ((read._webglType === "element") !== (write._webglType === "element")) error = GL_INVALID_OPERATION;
        else if (r + n > (read._size || 0) || w + n > (write._size || 0)) error = GL_INVALID_VALUE;
        else if (read === write && r < w + n && w < r + n) error = GL_INVALID_VALUE;
        if (error !== 0) {
            recordGpuPreflightError(this._canvasId, error);
            return;
        }
        if (write._webglType === "element") {
            TypedArrayPrototypeSet(write._elements, new Uint8Array(TypedArrayPrototypeGetBuffer(read._elements),
                TypedArrayPrototypeGetByteOffset(read._elements) + r, n), w);
            elementBytesChanged(write);
        }
        encodeCopyBufferSubData(this._canvasId, rt, wt, r, w, n);
    }
    renderbufferStorageMultisample(target, samples, internalformat, width, height) {
        const error = this._renderbufferStorageError(target, internalformat, width, height, samples);
        if (error !== 0) {
            recordGpuPreflightError(this._canvasId, error);
            return;
        }
        this._noteRenderbufferStorage(internalformat, width, height, Number(samples) >>> 0);
        _rawRenderbufferStorageMultisample(this._canvasId, target, samples,
                                            internalformat, width, height);
    }

    // ---- Sampler objects ---------------------------------------
    createSampler() {
        // op_alloc_gl_resource_id: direct, no-submit.
        const id = op_alloc_gl_resource_id_webgl2();
        _rawCreateSampler(this._canvasId, id);
        return new WebglObject(id, "sampler", this._canvasId);
    }
    // A deleted sampler is unbound from every unit it was bound to, as GL unbinds it.
    deleteSampler(sampler) {
        if (!this._isLive(sampler, "sampler")) return;
        sampler._deleted = true;
        for (const [index, bound] of this._samplerBindings) {
            if (bound !== sampler) continue;
            this._samplerBindings.delete(index);
            refreshUnitSampling(this, 0x84c0 + index);
        }
        _rawDeleteSampler(sampler._id);
    }
    isSampler(sampler) { return this._isLive(sampler, "sampler"); }
    // `bindSampler(unit, sampler)`: an object that is not a sampler is WebIDL's TypeError; a deleted sampler or another
    // context's INVALID_OPERATION; a unit past MAX_COMBINED_TEXTURE_IMAGE_UNITS INVALID_VALUE (ES 3.0 3.8.2). The
    // binding is recorded, so SAMPLER_BINDING answers with the object.
    bindSampler(unit, sampler) {
        const index = Number(unit) >>> 0;
        const bound = sampler === undefined ? null : sampler;
        if (bound !== null) {
            if (!(bound instanceof WebglObject) || bound._kind !== "sampler") {
                throw new TypeError("Failed to execute 'bindSampler' on 'WebGL2RenderingContext': parameter 2 is not of type 'WebGLSampler'.");
            }
            if (bound._deleted || bound._ownerId !== this._canvasId) {
                recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
                return;
            }
        }
        if (index >= this._textureUnitMinimum &&
                index >= this._cachedLimit("_maxTextureUnits", 0x8b4d, this._textureUnitMinimum)) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
            return;
        }
        if (bound === null) this._samplerBindings.delete(index);
        else this._samplerBindings.set(index, bound);
        refreshUnitSampling(this, 0x84c0 + index);
        const samplerId = bound ? bound._id : 0;
        // opcode 15: H C U U. unit is u32, samplerId is u32 (0 = unbind).
        if (typeof unit === "number") {
            encodeBindSampler(this._canvasId, unit >>> 0, samplerId >>> 0);
            return;
        }
        _rawBindSampler(this._canvasId, unit, samplerId);
    }
    // The checks `samplerParameteri` and `samplerParameterf` share: a live sampler of this context
    // (INVALID_OPERATION), a parameter a sampler has and, for an enum one, a value it takes (INVALID_ENUM). The value to
    // record, or undefined when refused.
    _samplerParameterValue(name, sampler, pname, value) {
        if (!(sampler instanceof WebglObject) || sampler._kind !== "sampler") {
            throw new TypeError(`Failed to execute '${name}' on 'WebGL2RenderingContext': parameter 1 is not of type 'WebGLSampler'.`);
        }
        if (!this._isLive(sampler, "sampler")) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return undefined;
        }
        const spec = this._samplerParameter(pname);
        if (spec === undefined || (spec.values !== null && !_listHas(spec.values, value))) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return undefined;
        }
        if (this._refusesAnisotropy(pname, value)) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
            return undefined;
        }
        return value;
    }
    samplerParameteri(sampler, pname, param) {
        const p = Number(pname) >>> 0;
        const value = this._samplerParameterValue("samplerParameteri", sampler, p, Number(param) | 0);
        if (value === undefined) return;
        (sampler._parameters || (sampler._parameters = new Map())).set(p, value);
        refreshSamplerSampling(this, sampler, p);
        // opcode 45: H U U I. No canvas field: a sampler is identified by its id.
        if (typeof pname === "number" && typeof param === "number") {
            encodeSamplerParameteri(sampler._id >>> 0, p, value);
            return;
        }
        _rawSamplerParameteri(sampler._id, pname, param);
    }
    samplerParameterf(sampler, pname, param) {
        const p = Number(pname) >>> 0;
        const f = MathFround(Number(param));
        // An enum parameter set through the float call takes the nearest integer (ES 3.0 2.3.1).
        const spec = this._samplerParameter(p);
        const value = this._samplerParameterValue("samplerParameterf", sampler, p, spec && spec.values !== null ? MathRound(f) : f);
        if (value === undefined) return;
        (sampler._parameters || (sampler._parameters = new Map())).set(p, value);
        refreshSamplerSampling(this, sampler, p);
        // opcode 46: H U U F.
        if (typeof pname === "number" && typeof param === "number") {
            encodeSamplerParameterf(sampler._id >>> 0, p, f);
            return;
        }
        _rawSamplerParameterf(sampler._id, pname, param);
    }
    getSamplerParameter(sampler, pname) {
        if (!(sampler instanceof WebglObject) || sampler._kind !== "sampler") {
            throw new TypeError("Failed to execute 'getSamplerParameter' on 'WebGL2RenderingContext': parameter 1 is not of type 'WebGLSampler'.");
        }
        if (!this._isLive(sampler, "sampler")) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return null;
        }
        const p = pname >>> 0;
        const spec = this._samplerParameter(p);
        if (spec === undefined) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return null;
        }
        const value = sampler._parameters ? sampler._parameters.get(p) : undefined;
        return value === undefined ? spec.initial : value;
    }

    // ---- Indexed buffer bindings ---------------------------------------------------------------------------------
    // `getIndexedParameter` answers from what `bindBufferBase` / `bindBufferRange` set: the uniform buffer bindings
    // are the context's, the transform feedback ones the bound transform feedback object's (ES 3.0 6.2). A bind is
    // recorded only when it is one the decoder and the driver take. The decoder checks the target, a transform feedback
    // that is capturing, a range's offset and size and its transform feedback alignment; the index past the binding
    // points and a uniform buffer offset off UNIFORM_BUFFER_OFFSET_ALIGNMENT only the driver would see, and its
    // error would not reach `getError`, so those two are INVALID_VALUE here, before anything is encoded.
    _indexedBindings(target) {
        return target === 0x8a11 ? this._uniformBufferBindings : this._transformFeedbackState().bindings;
    }
    // Whether `index` is past the target's binding points. An index below the minimum every implementation has
    // (24 uniform buffer bindings, 4 transform feedback ones) asks nothing.
    _indexedBindingLimit(target, index) {
        return target === 0x8a11
            ? index >= 24 && index >= this._cachedLimit("_maxUniformBufferBindings", 0x8a2f, 24)       // MAX_UNIFORM_BUFFER_BINDINGS
            : index >= 4 && index >= this._cachedLimit("_maxTransformFeedbackBindings", 0x8c8b, 4);   // MAX_TRANSFORM_FEEDBACK_SEPARATE_ATTRIBS
    }
    _transformFeedbackCaptures() {
        const state = this._transformFeedbackState();
        return state.active === true && state.paused !== true;
    }
    // Whether a bind passes what the decoder checks, so the record is made only of what takes effect.
    _indexedBindTakes(target, buffer, offset, size, range) {
        if (target !== 0x8a11 && target !== 0x8c8e) return false;
        if (target === 0x8c8e && this._transformFeedbackCaptures()) return false;
        if (!range || !buffer) return true;
        if (offset < 0 || size <= 0) return false;
        return target !== 0x8c8e || (offset % 4 === 0 && size % 4 === 0);
    }
    _recordIndexedBind(target, index, buffer, offset, size) {
        const bindings = this._indexedBindings(target);
        if (buffer) bindings.set(index, { buffer, offset, size }); else bindings.delete(index);
    }
    getIndexedParameter(target, index) {
        const t = target >>> 0;
        const i = index >>> 0;
        const which = t === 0x8c8f || t === 0x8c84 || t === 0x8c85 ? 0x8c8e     // TRANSFORM_FEEDBACK_BUFFER_{BINDING,START,SIZE}
            : t === 0x8a28 || t === 0x8a29 || t === 0x8a2a ? 0x8a11              // UNIFORM_BUFFER_{BINDING,START,SIZE}
            : 0;
        if (which === 0) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return null;
        }
        if (this._indexedBindingLimit(which, i)) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
            return null;
        }
        const binding = this._indexedBindings(which).get(i);
        if (t === 0x8c8f || t === 0x8a28) return binding ? binding.buffer : null;
        if (t === 0x8c84 || t === 0x8a29) return binding ? binding.offset : 0;
        return binding ? binding.size : 0;
    }

    // ---- Fence syncs -------------------------------------------
    fenceSync(condition, flags) {
        // op_alloc_gl_resource_id: direct, no-submit.
        const id = op_alloc_gl_resource_id_webgl2();
        _rawFenceSync(this._canvasId, id, condition, flags);
        const sync = new WebglObject(id, "sync", this._canvasId);
        sync._epoch = syncTaskEpoch(this._canvasId);
        return sync;
    }
    deleteSync(sync) {
        if (!this._isLive(sync, "sync")) return;
        sync._deleted = true;
        _rawDeleteSync(sync._id);
    }
    isSync(sync) { return this._isLive(sync, "sync"); }
    // A fence's type, condition and flags are what `fenceSync` makes (ES 3.0 4.1.1), and its status is a poll of the
    // fence (`clientWaitSync` with no timeout), as `clientWaitSync` itself answers.
    getSyncParameter(sync, pname) {
        if (!(sync instanceof WebglObject) || sync._kind !== "sync") {
            throw new TypeError("Failed to execute 'getSyncParameter' on 'WebGL2RenderingContext': parameter 1 is not of type 'WebGLSync'.");
        }
        if (!this._isLive(sync, "sync")) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return null;
        }
        switch (pname >>> 0) {
            case 0x9112: return 0x9116;                       // OBJECT_TYPE: SYNC_FENCE
            case 0x9113: return 0x9117;                       // SYNC_CONDITION: SYNC_GPU_COMMANDS_COMPLETE
            case 0x9115: return 0;                            // SYNC_FLAGS
            case 0x9114: {                                    // SYNC_STATUS
                if (sync._epoch === _syncTaskEpoch) return 0x9118;     // UNSIGNALED in the task that made it
                const result = _rawClientWaitSync(sync._id, 0);
                return result === 0x911a || result === 0x911c ? 0x9119 : 0x9118;   // ALREADY_SIGNALED / CONDITION_SATISFIED: SIGNALED
            }
            default:
                recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
                return null;
        }
    }
    // The GL server waits for the fence before it runs what follows. The flags and the timeout each have one legal
    // value (0, TIMEOUT_IGNORED): anything else is INVALID_VALUE. A sync deleted, or another context's, is
    // INVALID_OPERATION; a value that is not a WebGLSync is a TypeError.
    waitSync(sync, flags, timeout) {
        if (!(sync instanceof WebglObject) || sync._kind !== "sync") {
            throw new TypeError("Failed to execute 'waitSync' on 'WebGL2RenderingContext': parameter 1 is not of type 'WebGLSync'.");
        }
        const f = flags >>> 0;
        const t = toLongLong(timeout);
        if (sync._deleted || sync._ownerId !== this._canvasId) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return;
        }
        if (f !== 0 || t !== -1) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
            return;
        }
        _rawWaitSync(this._canvasId, sync._id);
    }
    /**
     * clientWaitSync(sync, flags, timeout) -- poll only.
     *
     * `timeout` is bounded by MAX_CLIENT_WAIT_TIMEOUT_WEBGL, which this context
     * reports as zero. That is what browsers report and the WebGL 2 conformance
     * suite requires only that it be non-negative and no greater than one second,
     * so zero is conformant and is what content written for the web already
     * assumes: the standard pattern is fence, then poll with timeout 0 on later
     * frames.
     *
     * Zero and not one second, because the timeout was previously unbounded and
     * honoured to its full 64-bit range on the render thread. That thread is
     * shared by every canvas and by the frame loop, so content asking for a long
     * wait stalled the whole engine -- while its own JavaScript thread was
     * blocked on the reply as well. Both threads, for as long as content asked.
     * Bounding it at zero removes the wait rather than shortening it.
     */
    clientWaitSync(sync, flags, timeout) {
        if (!sync || !sync._id) return 37149; // WAIT_FAILED
        if (sync._deleted) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return 37149; // WAIT_FAILED
        }
        // Not in the task that made it (see `syncTaskEpoch`).
        if (sync._epoch === _syncTaskEpoch) return 0x911b; // TIMEOUT_EXPIRED
        // Per the WebGL 2 specification: a timeout above the maximum is
        // INVALID_OPERATION, and the call returns WAIT_FAILED without doing
        // anything. Rejected here rather than clamped, so content is told rather
        // than silently given different semantics than it asked for.
        if ((Number(timeout) || 0) > 0) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return 37149; // WAIT_FAILED
        }
        return _rawClientWaitSync(sync._id, flags);
    }

    // ---- Draw / read buffer selection --------------------------
    // The draw framebuffer's draw buffers (ES 3.0 4.2.1, judged as a browser judges them): `drawBuffersOf`.
    drawBuffers(buffers) {
        drawBuffersOf(this, buffers);
    }

    // READ_BUFFER is the read framebuffer's: BACK or NONE for the default one, NONE or a colour attachment below
    // MAX_COLOR_ATTACHMENTS for an object (ES 3.0 4.3.1). Another of those names is INVALID_OPERATION, anything else
    // INVALID_ENUM. Recorded, as reads judge their source by it.
    readBuffer(src) {
        const b = Number(src) >>> 0;
        const fb = this._readFramebufferBinding;
        const color = b - 0x8ce0;
        let error = 0;
        if (b !== 0 && b !== 0x0405 && !(color >= 0 && color < 16)) error = GL_INVALID_ENUM;
        else if (fb === null ? b !== 0 && b !== 0x0405 : b === 0x0405 || (b !== 0 && !this._hasColorAttachment(color))) {
            error = GL_INVALID_OPERATION;
        }
        if (error !== 0) {
            recordGpuPreflightError(this._canvasId, error);
            return;
        }
        if (fb === null) this._defaultReadBuffer = b;
        else fb._readBuffer = b;
        // opcode 53: H C U.
        encodeReadBuffer(this._canvasId, b);
    }

    // ---- Query objects -----------------------------------------
    // A query is a WebglObject carrying the target it was first begun with (0 before), whether it is active, and the
    // answer it last gave, by the task it gave it in. The two occlusion targets share one slot: one occlusion query is
    // active at a time, whichever of them it was begun with (ES 3.0 2.14).
    createQuery() {
        // op_alloc_gl_resource_id: direct, no-submit.
        const id = op_alloc_gl_resource_id_webgl2();
        _rawCreateQuery(this._canvasId, id);
        const query = new WebglObject(id, "query", this._canvasId);
        query._target = 0;
        query._active = false;
        query._endEpoch = -1;       // the task it was last ended in
        query._answerEpoch = -1;    // the task its answer below was taken in
        query._available = false;
        query._result = 0;
        return query;
    }
    // An active query is ended as it is deleted.
    deleteQuery(query) {
        if (!this._isLive(query, "query")) return;
        query._deleted = true;
        if (query._active) {
            query._active = false;
            this._currentQueryByTarget.delete(_querySlot(query._target));
        }
        _rawDeleteQuery(query._id);
    }
    isQuery(query) { return this._isLive(query, "query") && query._target !== 0; }
    // `beginQuery(target, query)`: a target that is not a query target is INVALID_ENUM; an object that is not a query
    // WebIDL's TypeError; a deleted query or another context's, one already active, one first begun with another
    // target, or a target whose slot already has an active query, INVALID_OPERATION.
    beginQuery(target, query) {
        const t = Number(target) >>> 0;
        const slot = _querySlot(t);
        if (slot === undefined) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return;
        }
        if (!(query instanceof WebglObject) || query._kind !== "query") {
            throw new TypeError("Failed to execute 'beginQuery' on 'WebGL2RenderingContext': parameter 2 is not of type 'WebGLQuery'.");
        }
        if (query._deleted || query._ownerId !== this._canvasId || query._active ||
                (query._target !== 0 && query._target !== t) || this._currentQueryByTarget.has(slot)) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return;
        }
        query._target = t;
        query._active = true;
        query._available = false;      // a use begun again owes a result of its own
        query._result = 0;
        query._answerEpoch = -1;
        this._currentQueryByTarget.set(slot, query);
        _rawBeginQuery(this._canvasId, t, query._id);
    }
    // `endQuery(target)`: INVALID_ENUM for a target that is not one, INVALID_OPERATION when no query is active for it.
    // Its result is not available before the task ends (WebGL 2.0 5.38), which is also when the context is flushed.
    endQuery(target) {
        const t = Number(target) >>> 0;
        const slot = _querySlot(t);
        if (slot === undefined) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return;
        }
        const query = this._currentQueryByTarget.get(slot);
        if (query === undefined || query._target !== t) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return;
        }
        query._active = false;
        query._endEpoch = syncTaskEpoch(this._canvasId);
        this._currentQueryByTarget.delete(slot);
        _rawEndQuery(this._canvasId, t);
    }
    // CURRENT_QUERY: the query active for `target`, begun with that target.
    getQuery(target, pname) {
        const t = Number(target) >>> 0;
        const slot = _querySlot(t);
        if (slot === undefined || (Number(pname) >>> 0) !== GL_CURRENT_QUERY) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return null;
        }
        const query = this._currentQueryByTarget.get(slot);
        return query !== undefined && query._target === t ? query : null;
    }
    // QUERY_RESULT_AVAILABLE, a boolean, and QUERY_RESULT, the number. A query's result is not available in the task
    // that ended it, and within one task every ask answers the same (WebGL 2.0 5.38): the answer is taken once a task
    // and kept, and a result is asked for only once it is available -- QUERY_RESULT is 0 until then rather than a wait
    // on the GPU. An object that is not a query is a TypeError; a deleted query, another context's, one never begun or
    // one still active INVALID_OPERATION; another pname INVALID_ENUM.
    getQueryParameter(query, pname) {
        if (!(query instanceof WebglObject) || query._kind !== "query") {
            throw new TypeError("Failed to execute 'getQueryParameter' on 'WebGL2RenderingContext': parameter 1 is not of type 'WebGLQuery'.");
        }
        if (query._deleted || query._ownerId !== this._canvasId || query._target === 0 || query._active) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return null;
        }
        const p = Number(pname) >>> 0;
        if (p !== 0x8866 && p !== 0x8867) {      // QUERY_RESULT, QUERY_RESULT_AVAILABLE
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return null;
        }
        const epoch = syncTaskEpoch(this._canvasId);
        if (query._answerEpoch !== epoch && query._endEpoch !== epoch && !query._available) {
            query._answerEpoch = epoch;
            if (_rawGetQueryParameter(query._id, 0x8867) !== 0) {
                query._available = true;
                query._result = _rawGetQueryParameter(query._id, 0x8866);
            }
        }
        return p === 0x8867 ? query._available : query._result;
    }

    // ---- Transform Feedback ------------------------------------
    createTransformFeedback() {
        // op_alloc_gl_resource_id: direct, no-submit.
        const id = op_alloc_gl_resource_id_webgl2();
        _rawCreateTransformFeedback(this._canvasId, id);
        // The object is its own state: whether it is active and paused, and its buffers, as the default one's are in
        // `_defaultTransformFeedback`.
        const tf = new WebglObject(id, "transformFeedback", this._canvasId);
        tf._everBound = false;
        tf.active = false;
        tf.paused = false;
        tf.bindings = new Map();
        tf.genericBuffer = null;
        return tf;
    }
    // One that is active is not deleted (INVALID_OPERATION, ES 3.0 2.15.1); the bound one is replaced by the default.
    deleteTransformFeedback(tf) {
        if (!this._isLive(tf, "transformFeedback")) return;
        if (tf.active) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return;
        }
        tf._deleted = true;
        if (this._currentTransformFeedback === tf) {
            this._currentTransformFeedback = null;
        }
        _rawDeleteTransformFeedback(tf._id);
    }
    isTransformFeedback(tf) { return this._isLive(tf, "transformFeedback") && tf._everBound === true; }
    // `bindTransformFeedback(target, tf)`: a target other than TRANSFORM_FEEDBACK is INVALID_ENUM; an object that is
    // not a transform feedback WebIDL's TypeError; a deleted one or another context's INVALID_OPERATION, as is any
    // binding while the bound one is active and not paused (ES 3.0 2.15.1).
    bindTransformFeedback(target, tf) {
        const bound = tf === undefined ? null : tf;
        if ((Number(target) >>> 0) !== 0x8e22) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_ENUM);
            return;
        }
        if (bound !== null) {
            if (!(bound instanceof WebglObject) || bound._kind !== "transformFeedback") {
                throw new TypeError("Failed to execute 'bindTransformFeedback' on 'WebGL2RenderingContext': parameter 2 is not of type 'WebGLTransformFeedback'.");
            }
            if (bound._deleted || bound._ownerId !== this._canvasId) {
                recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
                return;
            }
        }
        const current = this._transformFeedbackState();
        if (current.active && !current.paused) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_OPERATION);
            return;
        }
        this._currentTransformFeedback = bound;
        if (bound !== null) bound._everBound = true;
        _rawBindTransformFeedback(this._canvasId, 0x8e22, bound ? bound._id : 0);
    }
    // Whether transform feedback is active and paused is the bound object's state -- the default object's too.
    beginTransformFeedback(primitiveMode) {
        const state = this._transformFeedbackState();
        state.active = true;
        state.paused = false;
        _rawBeginTransformFeedback(this._canvasId, primitiveMode);
    }
    endTransformFeedback() {
        const state = this._transformFeedbackState();
        state.active = false;
        state.paused = false;
        _rawEndTransformFeedback(this._canvasId);
    }
    pauseTransformFeedback() {
        const state = this._transformFeedbackState();
        if (state.active) state.paused = true;
        _rawPauseTransformFeedback(this._canvasId);
    }
    resumeTransformFeedback() {
        const state = this._transformFeedbackState();
        if (state.active) state.paused = false;
        _rawResumeTransformFeedback(this._canvasId);
    }
    /**
     * transformFeedbackVaryings(program, varyings, bufferMode)
     * The varyings array is sent as a single US-separated string
     * because the fast op lane accepts one `#[string]` argument
     * per call.  ASCII 0x1F (Unit Separator) is chosen because
     * it can't legally appear in GLSL identifiers.
     */
    transformFeedbackVaryings(program, varyings, bufferMode) {
        if (!program || !program._id) return;
        const joined = (varyings || []).join('\x1f');
        _rawTransformFeedbackVaryings(this._canvasId, program._id, joined, bufferMode);
    }
    getTransformFeedbackVarying(program, index) {
        if (!program || !program._id) return null;
        return this._activeInfo(
            this._transformFeedbackVaryingCache,
            _fetchTransformFeedbackVarying,
            program._id,
            index,
        );
    }

    // ---- 3D textures -------------------------------------------
    // `pixelsOrOffset` is a view (from its `srcOffset` element on), null (storage only), an offset into the bound
    // PIXEL_UNPACK_BUFFER, or a TexImageSource sliced into `depth` images (`_refusesSourceSelection`). Refused before
    // anything is sent as `texImage2D` is, against UNPACK_IMAGE_HEIGHT and the skipped rows too; and a view while an
    // unpack flag is set (`_refusesUnpackFlagsIn3D`), which a TexImageSource takes. The image is recorded once the
    // upload is sent.
    texImage3D(
        target, level, internalformat,
        width, height, depth, border,
        format, type, pixelsOrOffset, srcOffset
    ) {
        const fromSource = pixelsOrOffset != null && typeof pixelsOrOffset === "object" && !ArrayBufferIsView(pixelsOrOffset);
        const imageSource = fromSource ? requireTexImageSource(pixelsOrOffset, "texImage3D") : null;
        const texture = this._textureFor(target, "image3D");
        if (!texture) return;
        const fromBuffer = pixelsOrOffset != null && typeof pixelsOrOffset !== "object";
        if (fromBuffer ? this._refusesUnpackBufferSource()
            : this._refusesUnpackBufferBound() ||
                (pixelsOrOffset != null && !fromSource && this._refusesUnpackFlagsIn3D())) return;
        const maxXY = target === 0x806F
            ? MAX_WEBGL_GPU_3D_DIMENSION
            : MAX_WEBGL_GPU_2D_DIMENSION;
        const maxDepth = target === 0x806F
            ? MAX_WEBGL_GPU_3D_DIMENSION
            : MAX_WEBGL_GPU_ARRAY_LAYERS;
        if (!NumberIsInteger(level) || level < 0 || level >= maxMipLevels(maxXY)) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
            return;
        }
        let formatError = fromSource
            ? this._sourceUploadFormatError(internalformat, format, type)
            : this._uploadFormatError(internalformat, format, type);
        // ES 3.0 3.8.3: a depth or depth-stencil image is not a TEXTURE_3D's.
        const f = Number(format) >>> 0;
        if (formatError === 0 && target === 0x806F && (f === 0x1902 || f === 0x84F9)) formatError = GL_INVALID_OPERATION;
        if (formatError !== 0) {
            recordGpuPreflightError(this._canvasId, formatError);
            return;
        }
        const maxAtLevel = (maxXY >>> level) || 1;
        const maxDepthAtLevel = target === 0x806F
            ? ((maxDepth >>> level) || 1)
            : maxDepth;
        if (!NumberIsInteger(width) || width < 0 || width > maxAtLevel ||
            !NumberIsInteger(height) || height < 0 || height > maxAtLevel ||
            !NumberIsInteger(depth) || depth < 0 || depth > maxDepthAtLevel || border !== 0) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
            return;
        }
        if (this._refusesImmutable(texture)) return;
        if (fromSource) {
            if (this._refusesSourceSelection(imageSource, width, height, depth, true)) return;
            settleCanvasTextCache(this, imageSource, false);
            texImageFromSourceOf(
                this, TEX_SOURCE_CALL_IMAGE_3D, target, level, internalformat, 0, 0, 0, width, height, depth,
                format, type, 0, imageSource,
            );
        } else {
            const source = this._upload3DSource(pixelsOrOffset, srcOffset, fromBuffer, width, height, depth, format, type, true);
            if (source === null) return;
            _rawTexImage3D(
                this._canvasId, target, level, internalformat,
                width, height, depth, border, format, type,
                source[0], source[1],
            );
        }
        defineTextureImage(texture, target, level,
            new TextureImage(Number(internalformat) >>> 0, f, Number(type) >>> 0, width, height, depth, false));
        refreshTextureSampling(this, texture);
    }
    // As `texImage3D`, over the (format, type) pairs of either table, then against the image the upload goes into
    // (`_refusesSubImage`). Null pixels are INVALID_VALUE, and FLOAT_32_UNSIGNED_INT_24_8_REV from anything but a buffer
    // INVALID_ENUM (WebGL 2.0 3.7.6).
    texSubImage3D(
        target, level,
        xoffset, yoffset, zoffset,
        width, height, depth,
        format, type, pixelsOrOffset, srcOffset
    ) {
        const fromSource = pixelsOrOffset != null && typeof pixelsOrOffset === "object" && !ArrayBufferIsView(pixelsOrOffset);
        const imageSource = fromSource ? requireTexImageSource(pixelsOrOffset, "texSubImage3D") : null;
        const texture = this._textureFor(target, "image3D");
        if (!texture) return;
        const fromBuffer = pixelsOrOffset != null && typeof pixelsOrOffset !== "object";
        if (fromBuffer ? this._refusesUnpackBufferSource()
            : this._refusesUnpackBufferBound() ||
                (pixelsOrOffset != null && !fromSource && this._refusesUnpackFlagsIn3D())) return;
        const levels = maxMipLevels(target === 0x806F ? MAX_WEBGL_GPU_3D_DIMENSION : MAX_WEBGL_GPU_2D_DIMENSION);
        const formatError = fromSource ? this._sourceSubUploadFormatError(format, type)
            : !fromBuffer && (Number(type) >>> 0) === 0x8dad ? GL_INVALID_ENUM : this._subUploadFormatError(format, type);
        if (!preflightTexSubImage(
            this._canvasId, level, levels, formatError, xoffset, yoffset, zoffset, width, height, depth,
        )) return;
        if (pixelsOrOffset == null) {
            recordGpuPreflightError(this._canvasId, GL_INVALID_VALUE);
            return;
        }
        if (fromSource) {
            if (this._refusesSourceSelection(imageSource, width, height, depth, true) ||
                this._refusesSubImage(texture, target, level, format, type, xoffset, yoffset, zoffset, width, height, depth)) return;
            settleCanvasTextCache(this, imageSource, false);
            texImageFromSourceOf(
                this, TEX_SOURCE_CALL_SUB_IMAGE_3D, target, level, 0, xoffset, yoffset, zoffset, width, height, depth,
                format, type, _sourceCopyFormat(this._image(texture, Number(target) >>> 0, level)), imageSource,
            );
            return;
        }
        const source = this._upload3DSource(pixelsOrOffset, srcOffset, fromBuffer, width, height, depth, format, type, false);
        if (source === null ||
            this._refusesSubImage(texture, target, level, format, type, xoffset, yoffset, zoffset, width, height, depth)) return;
        _rawTexSubImage3D(
            this._canvasId, target, level,
            xoffset, yoffset, zoffset,
            width, height, depth, format, type,
            source[0], source[1],
        );
    }
    // A 3D upload's source as the op's last two arguments -- the bytes (none: null for storage only, empty with an
    // offset) and the PIXEL_UNPACK_BUFFER offset (-1 for none) -- or null when refused with the error recorded.
    // `reserves`: null pixels allocate storage.
    _upload3DSource(pixelsOrOffset, srcOffset, fromBuffer, width, height, depth, format, type, reserves) {
        if (fromBuffer) {
            const offset = this._unpackBufferOffset(pixelsOrOffset, width, height, depth, format, type, true);
            return offset < 0 ? null : [EMPTY_UPLOAD_BYTES, offset];
        }
        if (pixelsOrOffset === null || pixelsOrOffset === undefined) return reserves ? [null, -1] : null;
        if (this._refusesUnpackRegion(width, height, true)) return null;
        const bytes = this._uploadViewBytes(pixelsOrOffset, srcOffset, width, height, depth, format, type, true);
        return bytes === null ? null : [bytes, -1];
    }
    // ---- Compressed uploads (WebGL 2) --------------------------------------------------------------------------------
    // Each takes a view with `srcOffset` / `srcLengthOverride`, or `imageSize` / `offset` into the bound
    // PIXEL_UNPACK_BUFFER: see `_compressedUploadSource`.
    // Then refused as `_acceptsCompressedImage` / `_acceptsCompressedSubImage` describe, the data's length being the
    // view range's or `imageSize`; an image is recorded once its upload is sent.
    compressedTexImage2D(target, level, internalformat, width, height, border, dataOrSize, srcOffsetOrOffset = 0, srcLengthOverride = 0) {
        const texture = this._textureFor(target, "image2D");
        if (!texture) return;
        const source = this._compressedUploadSource(dataOrSize, srcOffsetOrOffset, srcLengthOverride);
        if (source === null || !this._acceptsCompressedImage(
            texture, target, level, internalformat, width, height, 1, border, _compressedSourceBytes(source),
        )) return;
        _rawCompressedTexImage2D(this._canvasId, target, level, internalformat, width, height, border, source[0], source[1], source[2]);
        defineCompressedTextureImage(texture, target, level, internalformat, width, height, 1);
        refreshTextureSampling(this, texture);
    }
    compressedTexSubImage2D(target, level, xoffset, yoffset, width, height, format, dataOrSize, srcOffsetOrOffset = 0, srcLengthOverride = 0) {
        const texture = this._textureFor(target, "image2D");
        if (!texture) return;
        const source = this._compressedUploadSource(dataOrSize, srcOffsetOrOffset, srcLengthOverride);
        if (source === null || !this._acceptsCompressedSubImage(
            texture, target, level, xoffset, yoffset, 0, width, height, 1, format, _compressedSourceBytes(source),
        )) return;
        _rawCompressedTexSubImage2D(this._canvasId, target, level, xoffset, yoffset, width, height, format, source[0], source[1], source[2]);
    }
    compressedTexImage3D(target, level, internalformat, width, height, depth, border, dataOrSize, srcOffsetOrOffset = 0, srcLengthOverride = 0) {
        const texture = this._textureFor(target, "image3D");
        if (!texture) return;
        const source = this._compressedUploadSource(dataOrSize, srcOffsetOrOffset, srcLengthOverride);
        if (source === null || !this._acceptsCompressedImage(
            texture, target, level, internalformat, width, height, depth, border, _compressedSourceBytes(source),
        )) return;
        _rawCompressedTexImage3D(this._canvasId, target, level, internalformat, width, height, depth, border, source[0], source[1], source[2]);
        defineCompressedTextureImage(texture, target, level, internalformat, width, height, depth);
        refreshTextureSampling(this, texture);
    }
    compressedTexSubImage3D(target, level, xoffset, yoffset, zoffset, width, height, depth, format, dataOrSize, srcOffsetOrOffset = 0, srcLengthOverride = 0) {
        const texture = this._textureFor(target, "image3D");
        if (!texture) return;
        const source = this._compressedUploadSource(dataOrSize, srcOffsetOrOffset, srcLengthOverride);
        if (source === null || !this._acceptsCompressedSubImage(
            texture, target, level, xoffset, yoffset, zoffset, width, height, depth, format, _compressedSourceBytes(source),
        )) return;
        _rawCompressedTexSubImage3D(this._canvasId, target, level, xoffset, yoffset, zoffset, width, height, depth, format, source[0], source[1], source[2]);
    }
    texStorage3D(target, levels, internalformat, width, height, depth) {
        const texture = this._textureFor(target, "image3D");
        if (!texture) return;
        if (!preflightTexStorage3D(
            this._canvasId, target, levels, this._storageFormatError(internalformat, target), width, height, depth,
        ) || this._refusesImmutable(texture)) return;
        _rawTexStorage3D(
            this._canvasId, target, levels, internalformat,
            width, height, depth,
        );
        defineTextureStorage(texture, target, levels, internalformat, width, height, depth);
        refreshTextureSampling(this, texture);
    }
}

// GL batch flush is now handled by the unified frame-end hook in
// 02_2d_context.js (op_frame_end_unified). No separate GL hook needed.

// `WebGL2RenderingContext extends WebGLRenderingContext` is an implementation
// convenience -- WebGL2 really is a superset and sharing the method table is the
// right call. But the WebIDL says the two interfaces are *siblings*: in a
// browser `gl2 instanceof WebGLRenderingContext` is **false**, and code relies
// on that to tell the versions apart.
//
// Emscripten does exactly this. To work around a Safari bug it wraps
// `canvas.getContext` and validates the result with
// `(ver == 'webgl') == (gl instanceof WebGLRenderingContext)`. With plain
// prototype inheritance that reads `false == true` for a perfectly good WebGL2
// context, so the wrapper returns null and `emscripten_webgl_create_context`
// returns 0. The symptom is "no WebGL2 context" from content that is not
// wrong -- every Unity/Emscripten export using WebGL2 hits it, and nothing in
// the engine logs anything.
//
// So keep the inheritance and correct the *observable* relation: a WebGL2
// context is not an instance of WebGLRenderingContext, exactly as the IDL says.
// Written with `isPrototypeOf`, never `instanceof`: `WebGL2RenderingContext`
// *inherits* this very trap from its base, so an `instanceof` inside it
// recurses until the stack ends (observed: "Maximum call stack size exceeded"
// before any frame rendered). `WebGL2RenderingContext` also gets its own trap
// so it does not answer through this one.
Object.defineProperty(WebGLRenderingContext, Symbol.hasInstance, {
    value: (instance) =>
        WebGLRenderingContext.prototype.isPrototypeOf(instance)
        && !WebGL2RenderingContext.prototype.isPrototypeOf(instance),
    configurable: true,
});
Object.defineProperty(WebGL2RenderingContext, Symbol.hasInstance, {
    value: (instance) => WebGL2RenderingContext.prototype.isPrototypeOf(instance),
    configurable: true,
});

export {
    WebGLRenderingContext,
    WebGL2RenderingContext,
    _bumpCapabilityGeneration,
};
