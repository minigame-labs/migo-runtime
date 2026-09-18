// The WebGL queries: the calls whose return value is the answer.
//
// `getShaderParameter(shader, COMPILE_STATUS)`, `getUniformLocation`,
// `getError` -- a WebGL program makes these between recording work and drawing
// with it, and there is no answer to give locally. In the embedded runtime each
// is an op that flushes the pending command stream and blocks on the render
// thread. Here it is the same shape across a process boundary: the producer
// sends what it has recorded as a BARRIER (executed, not presented), then
// blocks in a synchronous request naming that barrier's sequence, and the host
// answers once the barrier has been admitted and run.
//
// WHY THE BARRIER MATTERS. `getShaderParameter(shader, COMPILE_STATUS)` asks
// about a shader whose source and compile are records in the frame being built.
// Without the barrier the host would answer about a shader it has not compiled
// -- and Three.js asks exactly this the first time it uses a material, inside
// the rAF callback that created it. Presenting the half-frame instead would
// flash it onto the screen, which is what barrier packets exist to avoid.
//
// THE COST IS A ROUND TRIP, and the producer does not hide it: each query is a
// packet out and a blocked Worker until the renderer answers. That is what the
// embedded runtime pays too (`send_gl_sync_with_flush`), and it is why the
// engine's facade caches locations rather than asking per draw.

import { engineHost } from "./engine-host.mjs";
import { flushToHost } from "./engine-frames.mjs";
import {
  ACTIVE_VARIABLE_HEADER_BYTES,
  GL_QUERY_ACTIVE_ATTRIB,
  GL_QUERY_ACTIVE_UNIFORM,
  GL_QUERY_ATTRIB_LOCATION,
  GL_QUERY_CHECK_FRAMEBUFFER_STATUS,
  GL_QUERY_CLIENT_WAIT_SYNC,
  GL_QUERY_GET_ERROR,
  GL_QUERY_PARAMETER,
  GL_QUERY_PROGRAM_INFO_LOG,
  GL_QUERY_PROGRAM_PARAMETER,
  GL_QUERY_QUERY_PARAMETER,
  GL_QUERY_SHADER_INFO_LOG,
  GL_QUERY_SHADER_PARAMETER,
  GL_QUERY_TRANSFORM_FEEDBACK_VARYING,
  GL_QUERY_UNIFORM_BLOCK_INDEX,
  GL_QUERY_UNIFORM_LOCATION,
  SYNC_OP_GL_QUERY_ACTIVE,
  SYNC_OP_GL_QUERY_SCALAR,
  SYNC_OP_GL_QUERY_TEXT,
  decodeActiveReply,
  decodeScalarReply,
  decodeTextReply,
  encodeGlQueryParams,
} from "./sync-mailbox.mjs";

/// How long a query waits before the frame channel is treated as stalled. The
/// same minute a blocked producer gives the window: a renderer answers a query
/// in microseconds, and a minute without one is a host that is not rendering.
const QUERY_TIMEOUT_MILLIS = 60_000;

/// What a text answer may be. An info log is a compiler's complaint about one
/// shader; a driver that produced more than this has produced a denial of
/// service rather than a message anyone reads.
const MAX_TEXT_REPLY_BYTES = 64 * 1024;

/// The reply an active variable needs: the two numbers and a name no longer
/// than a GLSL identifier.
const MAX_ACTIVE_REPLY_BYTES = ACTIVE_VARIABLE_HEADER_BYTES + 1024;

/**
 * Make the host execute what is recorded, then ask it one question.
 *
 * The flush is what makes the answer about the frame content just described:
 * it returns the sequence of the barrier it sent, and the host holds the answer
 * until that sequence has been admitted and run.
 */
function ask(operation, maxReplyBytes, params) {
  const host = engineHost();
  if (host.sync === undefined) {
    const error = new Error(
      "this producer has no synchronous endpoint, and a WebGL query has no answer without one",
    );
    error.name = "SyncUnavailable";
    throw error;
  }
  const triggeringSequence = flushToHost();
  const { state } = host;
  return host.sync.call({
    runtimeGeneration: host.runtimeGeneration,
    surfaceGeneration: state.surfaceGeneration,
    resourceEpoch: state.resourceEpoch,
    triggeringSequence,
    operation,
    maxReplyBytes,
    timeoutMillis: QUERY_TIMEOUT_MILLIS,
    params,
  });
}

function scalar(query) {
  return decodeScalarReply(ask(SYNC_OP_GL_QUERY_SCALAR, 4, encodeGlQueryParams(query)));
}

function text(query) {
  return decodeTextReply(ask(SYNC_OP_GL_QUERY_TEXT, MAX_TEXT_REPLY_BYTES, encodeGlQueryParams(query)));
}

/**
 * An active variable, as the op that this stands in for returns it: the JSON
 * object the engine's facade parses, or an empty string for an index the
 * program does not have.
 */
function activeVariable(kind, canvasId, program, index) {
  const found = decodeActiveReply(
    ask(SYNC_OP_GL_QUERY_ACTIVE, MAX_ACTIVE_REPLY_BYTES, encodeGlQueryParams({ kind, canvasId, object: program, pname: index })),
  );
  if (found === null) return "";
  return JSON.stringify({ name: found.name, size: found.size, type: found.type });
}

// ---- program and shader state ----------------------------------------------

export function op_get_program_parameter(programId, pname) {
  return scalar({ kind: GL_QUERY_PROGRAM_PARAMETER, object: programId, pname });
}

export function op_get_shader_parameter(shaderId, pname) {
  return scalar({ kind: GL_QUERY_SHADER_PARAMETER, object: shaderId, pname });
}

export function op_get_program_info_log(programId) {
  return text({ kind: GL_QUERY_PROGRAM_INFO_LOG, object: programId });
}

export function op_get_shader_info_log(shaderId) {
  return text({ kind: GL_QUERY_SHADER_INFO_LOG, object: shaderId });
}

// ---- locations and indices --------------------------------------------------

export function op_get_uniform_location(canvasId, programId, name) {
  return scalar({ kind: GL_QUERY_UNIFORM_LOCATION, canvasId, object: programId, name });
}

export function op_get_attrib_location(canvasId, programId, name) {
  return scalar({ kind: GL_QUERY_ATTRIB_LOCATION, canvasId, object: programId, name });
}

export function op_get_uniform_block_index(programId, name) {
  // The op is `#[smi] u32`: an index, and `INVALID_INDEX` (0xFFFFFFFF) when
  // there is none, which is the same bits the scalar reply carries.
  return scalar({ kind: GL_QUERY_UNIFORM_BLOCK_INDEX, object: programId, name }) >>> 0;
}

// ---- active variables -------------------------------------------------------

export function op_get_active_attrib(canvasId, programId, index) {
  return activeVariable(GL_QUERY_ACTIVE_ATTRIB, canvasId, programId, index);
}

export function op_get_active_uniform(canvasId, programId, index) {
  return activeVariable(GL_QUERY_ACTIVE_UNIFORM, canvasId, programId, index);
}

export function op_get_transform_feedback_varying(program, index) {
  return activeVariable(GL_QUERY_TRANSFORM_FEEDBACK_VARYING, 0, program, index);
}

// ---- context state ----------------------------------------------------------

export function op_get_parameter(canvasId, pname) {
  return text({ kind: GL_QUERY_PARAMETER, canvasId, pname });
}

export function op_check_framebuffer_status(canvasId, target) {
  return scalar({ kind: GL_QUERY_CHECK_FRAMEBUFFER_STATUS, canvasId, pname: target }) >>> 0;
}

export function op_get_query_parameter(query, pname) {
  return scalar({ kind: GL_QUERY_QUERY_PARAMETER, object: query, pname }) >>> 0;
}

export function op_client_wait_sync(sync, flags) {
  return scalar({ kind: GL_QUERY_CLIENT_WAIT_SYNC, object: sync, pname: flags }) >>> 0;
}

/**
 * `gl.getError()`.
 *
 * The queue it drains is the host's -- filled while the host decoded this
 * producer's own records, which is where a record's `INVALID_VALUE` is
 * recorded -- so the answer comes back without the renderer being asked. The
 * barrier still goes first: an error made by the frame being asked about has to
 * be in the queue before it is read.
 */
export function op_webgl_get_error(canvasId) {
  return scalar({ kind: GL_QUERY_GET_ERROR, canvasId }) >>> 0;
}
