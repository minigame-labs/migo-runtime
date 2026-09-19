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

import { constructOpError } from "./engine-core.mjs";
import { engineHost } from "./engine-host.mjs";
import {
  fileBuffer,
  fileStat,
  optionalU64Value,
  readIntoSync,
  requiredModule,
  statResult,
  writeData,
} from "./files.mjs";
import {
  bytesOf,
  optionalBytesOf,
  optionalStringOf,
  optionalU64,
  smiU32,
  smiU64,
  stringOf,
  toBool,
  toF64,
  toU64,
} from "./op-args.mjs";
import { flushToHost } from "./engine-frames.mjs";
import { decodeServiceOutcome, encodeServiceCall } from "./service.mjs";
import { SERVICE_OP } from "./service-ops.mjs";
import {
  ACTIVE_VARIABLE_HEADER_BYTES,
  CANVAS2D_FLAG_BOLD,
  CANVAS2D_FLAG_ITALIC,
  CANVAS2D_QUERY_MEASURE_TEXT,
  CANVAS2D_QUERY_TEXT_LINE_HEIGHT,
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
  SYNC_OP_CANVAS2D_METRICS,
  SYNC_OP_CANVAS2D_NUMBER,
  SYNC_OP_GL_QUERY_ACTIVE,
  SYNC_OP_GL_QUERY_SCALAR,
  SYNC_OP_GL_QUERY_TEXT,
  SYNC_OP_SERVICE,
  MAX_SERVICE_REPLY_BYTES,
  TEXT_METRICS_BYTES,
  decodeActiveReply,
  decodeMetricsReply,
  decodeNumberReply,
  encodeCanvas2DQueryParams,
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
  return scalar({
    kind: GL_QUERY_PROGRAM_PARAMETER,
    object: smiU32(programId, "program_id"),
    pname: smiU32(pname, "pname"),
  });
}

export function op_get_shader_parameter(shaderId, pname) {
  return scalar({
    kind: GL_QUERY_SHADER_PARAMETER,
    object: smiU32(shaderId, "shader_id"),
    pname: smiU32(pname, "pname"),
  });
}

export function op_get_program_info_log(programId) {
  return text({ kind: GL_QUERY_PROGRAM_INFO_LOG, object: smiU32(programId, "program_id") });
}

export function op_get_shader_info_log(shaderId) {
  return text({ kind: GL_QUERY_SHADER_INFO_LOG, object: smiU32(shaderId, "shader_id") });
}

// ---- locations and indices --------------------------------------------------

export function op_get_uniform_location(canvasId, programId, name) {
  return scalar({
    kind: GL_QUERY_UNIFORM_LOCATION,
    canvasId: smiU32(canvasId, "canvas_id"),
    object: smiU32(programId, "program_id"),
    name: stringOf(name, "name"),
  });
}

export function op_get_attrib_location(canvasId, programId, name) {
  return scalar({
    kind: GL_QUERY_ATTRIB_LOCATION,
    canvasId: smiU32(canvasId, "canvas_id"),
    object: smiU32(programId, "program_id"),
    name: stringOf(name, "name"),
  });
}

export function op_get_uniform_block_index(programId, name) {
  // The op is `#[smi] u32`: an index, and `INVALID_INDEX` (0xFFFFFFFF) when
  // there is none, which is the same bits the scalar reply carries.
  return (
    scalar({
      kind: GL_QUERY_UNIFORM_BLOCK_INDEX,
      object: smiU32(programId, "program_id"),
      name: stringOf(name, "name"),
    }) >>> 0
  );
}

// ---- active variables -------------------------------------------------------

export function op_get_active_attrib(canvasId, programId, index) {
  return activeVariable(
    GL_QUERY_ACTIVE_ATTRIB,
    smiU32(canvasId, "canvas_id"),
    smiU32(programId, "program_id"),
    smiU32(index, "index"),
  );
}

export function op_get_active_uniform(canvasId, programId, index) {
  return activeVariable(
    GL_QUERY_ACTIVE_UNIFORM,
    smiU32(canvasId, "canvas_id"),
    smiU32(programId, "program_id"),
    smiU32(index, "index"),
  );
}

export function op_get_transform_feedback_varying(program, index) {
  return activeVariable(GL_QUERY_TRANSFORM_FEEDBACK_VARYING, 0, smiU32(program, "program"), smiU32(index, "index"));
}

// ---- context state ----------------------------------------------------------

export function op_get_parameter(canvasId, pname) {
  return text({ kind: GL_QUERY_PARAMETER, canvasId: smiU32(canvasId, "canvas_id"), pname: smiU32(pname, "pname") });
}

export function op_check_framebuffer_status(canvasId, target) {
  return (
    scalar({
      kind: GL_QUERY_CHECK_FRAMEBUFFER_STATUS,
      canvasId: smiU32(canvasId, "canvas_id"),
      pname: smiU32(target, "target"),
    }) >>> 0
  );
}

export function op_get_query_parameter(query, pname) {
  return (
    scalar({ kind: GL_QUERY_QUERY_PARAMETER, object: smiU32(query, "query"), pname: smiU32(pname, "pname") }) >>> 0
  );
}

export function op_client_wait_sync(sync, flags) {
  return (
    scalar({ kind: GL_QUERY_CLIENT_WAIT_SYNC, object: smiU32(sync, "sync"), pname: smiU32(flags, "flags") }) >>> 0
  );
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
  return scalar({ kind: GL_QUERY_GET_ERROR, canvasId: smiU32(canvasId, "canvas_id") }) >>> 0;
}

// ---- Canvas2D ---------------------------------------------------------------

/**
 * `measureText(text)`, as the flat op answers it: twelve `f32`.
 *
 * The measurement is of the canvas's *current* font, which is a record in the
 * frame being built -- so the barrier goes first, as it does for every query
 * here, and the renderer measures against the font it has by then applied. The
 * shorthand crosses too, because the op takes it and a host with a JS-thread
 * measurer installed uses it; this host measures on the render thread.
 */
export function op_measure_text_flat(canvasId, text, cssFont) {
  const reply = ask(
    SYNC_OP_CANVAS2D_METRICS,
    TEXT_METRICS_BYTES,
    encodeCanvas2DQueryParams({
      kind: CANVAS2D_QUERY_MEASURE_TEXT,
      canvasId: smiU32(canvasId, "canvas_id"),
      text: stringOf(text, "text"),
      font: stringOf(cssFont, "css_font"),
    }),
  );
  return decodeMetricsReply(reply);
}

/** The line height of a family at a size, which the font loader caches. */
export function op_get_text_line_height(fontFamily, fontSize, bold, italic) {
  const family = stringOf(fontFamily, "font_family");
  const size = toF64(fontSize, "font_size");
  const flags =
    (toBool(bold, "bold") ? CANVAS2D_FLAG_BOLD : 0) | (toBool(italic, "italic") ? CANVAS2D_FLAG_ITALIC : 0);
  const reply = ask(
    SYNC_OP_CANVAS2D_NUMBER,
    8,
    encodeCanvas2DQueryParams({
      kind: CANVAS2D_QUERY_TEXT_LINE_HEIGHT,
      number: size,
      flags,
      font: family,
    }),
  );
  return decodeNumberReply(reply);
}

// ---- service ops called synchronously ---------------------------------------
//
// `getStorageSync`, `readFileSync`: service ops whose return value is the
// answer. What content asked for on the service stream before the call is sent
// first, and the call names it (`serviceSequence`), so the host runs this after
// it -- a `readFileSync` never overtakes the `writeFile` in front of it. No
// barrier: these are not about the frame being built.

/**
 * Run one service op on the host, blocked, and return its value.
 *
 * The op's own failure -- a missing file, a full quota -- comes back as an
 * answer and is thrown here as the class the engine registered under its name;
 * the barrier failing (a timeout, a session that ended) is thrown by the caller.
 */
export function callService(op, writeArgs) {
  const host = engineHost();
  if (host.sync === undefined || host.services === undefined) {
    const error = new Error(
      "this producer has no synchronous service endpoint, and the op has no answer without one",
    );
    error.name = "ServiceUnavailable";
    throw error;
  }
  const serviceSequence = host.services.flush();
  const { state } = host;
  const reply = host.sync.call({
    runtimeGeneration: host.runtimeGeneration,
    surfaceGeneration: state.surfaceGeneration,
    resourceEpoch: state.resourceEpoch,
    triggeringSequence: 0n,
    operation: SYNC_OP_SERVICE,
    maxReplyBytes: MAX_SERVICE_REPLY_BYTES,
    timeoutMillis: QUERY_TIMEOUT_MILLIS,
    serviceSequence,
    params: encodeServiceCall(op, writeArgs),
  });
  const outcome = decodeServiceOutcome(reply);
  if (outcome.ok) return outcome.value;
  throw constructOpError(outcome.className, outcome.message);
}

// ---- storage ------------------------------------------------------------------

export function op_storage_get(key) {
  const k = stringOf(key, "key");
  return callService(SERVICE_OP.op_storage_get, (w) => w.str(k));
}

export function op_storage_set(key, value) {
  const k = stringOf(key, "key");
  const v = stringOf(value, "value");
  callService(SERVICE_OP.op_storage_set, (w) => {
    w.str(k);
    w.str(v);
  });
}

export function op_storage_remove(key) {
  const k = stringOf(key, "key");
  callService(SERVICE_OP.op_storage_remove, (w) => w.str(k));
}

export function op_storage_clear() {
  callService(SERVICE_OP.op_storage_clear);
}

export function op_storage_info() {
  return callService(SERVICE_OP.op_storage_info);
}

export function op_create_buffer_url(buffer) {
  const bytes = bytesOf(buffer, "buffer");
  return callService(SERVICE_OP.op_create_buffer_url, (w) => w.bytes(bytes));
}

export function op_revoke_buffer_url(url) {
  const u = stringOf(url, "url");
  callService(SERVICE_OP.op_revoke_buffer_url, (w) => w.str(u));
}

// ---- images -------------------------------------------------------------------

/// This game's image cache figures, as the embedded op's serde answer.
export function op_get_image_cache_stats() {
  return callService(SERVICE_OP.op_get_image_cache_stats);
}

// ---- files ----------------------------------------------------------------------
//
// `readFileSync`, `statSync`, `openSync`: the synchronous file system, each a
// service call the Worker blocks on, answered on the host by the same
// `migo_services::fs` function the embedded op calls.

export function op_access_sync(path) {
  const p = stringOf(path, "path");
  return callService(SERVICE_OP.op_access_sync, (w) => w.str(p));
}

export function op_write_or_append_file_sync(path, dataBuf, dataStr, encoding, append, durable) {
  const p = stringOf(path, "path");
  const bytes = optionalBytesOf(dataBuf, "data_buf");
  const text = optionalStringOf(dataStr, "data_str");
  const enc = optionalStringOf(encoding, "encoding");
  const a = toBool(append, "append");
  const d = toBool(durable, "durable");
  if (bytes !== null) fileBuffer(bytes);
  return callService(SERVICE_OP.op_write_or_append_file_sync, (w) => {
    w.str(p);
    writeData(w, bytes, text, enc);
    w.bool(a);
    w.bool(d);
  });
}

export function op_open_file_sync(path, flag) {
  const p = stringOf(path, "path");
  const f = stringOf(flag, "flag");
  return callService(SERVICE_OP.op_open_file_sync, (w) => {
    w.str(p);
    w.str(f);
  });
}

export function op_close_file_sync(rid) {
  const fd = smiU32(rid, "rid");
  callService(SERVICE_OP.op_close_file_sync, (w) => w.u32(fd));
}

export function op_copy_file_sync(srcPath, destPath) {
  const src = stringOf(srcPath, "src_path");
  const dest = stringOf(destPath, "dest_path");
  callService(SERVICE_OP.op_copy_file_sync, (w) => {
    w.str(src);
    w.str(dest);
  });
}

export function op_fstat_sync(rid) {
  const fd = smiU32(rid, "rid");
  return fileStat(callService(SERVICE_OP.op_fstat_sync, (w) => w.u32(fd)));
}

export function op_ftruncate_sync(rid, len) {
  const fd = smiU32(rid, "rid");
  const length = smiU64(len, "len");
  callService(SERVICE_OP.op_ftruncate_sync, (w) => {
    w.u32(fd);
    w.u64(length);
  });
}

export function op_mkdir_sync(dirPath, recursive) {
  const p = stringOf(dirPath, "dir_path");
  const r = toBool(recursive, "recursive");
  callService(SERVICE_OP.op_mkdir_sync, (w) => {
    w.str(p);
    w.bool(r);
  });
}

export function op_readdir_sync(dirPath) {
  const p = stringOf(dirPath, "dir_path");
  return callService(SERVICE_OP.op_readdir_sync, (w) => w.str(p));
}

export function op_unlink_sync(filePath) {
  const p = stringOf(filePath, "file_path");
  callService(SERVICE_OP.op_unlink_sync, (w) => w.str(p));
}

export function op_rename_sync(oldPath, newPath) {
  const from = stringOf(oldPath, "old_path");
  const to = stringOf(newPath, "new_path");
  callService(SERVICE_OP.op_rename_sync, (w) => {
    w.str(from);
    w.str(to);
  });
}

export function op_rmdir_sync(dirPath, recursive) {
  const p = stringOf(dirPath, "dir_path");
  const r = toBool(recursive, "recursive");
  callService(SERVICE_OP.op_rmdir_sync, (w) => {
    w.str(p);
    w.bool(r);
  });
}

export function op_stat_sync(path, recursive) {
  const p = stringOf(path, "path");
  const r = toBool(recursive, "recursive");
  return statResult(
    callService(SERVICE_OP.op_stat_sync, (w) => {
      w.str(p);
      w.bool(r);
    }),
  );
}

/// The count written, as a BigInt: the op is `#[bigint]`.
export function op_write_file_sync(rid, dataBuf, dataStr, encoding, position) {
  const fd = smiU32(rid, "rid");
  const bytes = optionalBytesOf(dataBuf, "data_buf");
  const text = optionalStringOf(dataStr, "data_str");
  const enc = optionalStringOf(encoding, "encoding");
  const at = optionalU64(position, "position");
  if (bytes !== null) fileBuffer(bytes);
  return callService(SERVICE_OP.op_write_file_sync, (w) => {
    w.u32(fd);
    writeData(w, bytes, text, enc);
    optionalU64Value(w, at);
  });
}

export function op_read_file_sync(path, position, length) {
  const p = stringOf(path, "path");
  const at = optionalU64(position, "position");
  const len = optionalU64(length, "length");
  return callService(SERVICE_OP.op_read_file_sync, (w) => {
    w.str(p);
    optionalU64Value(w, at);
    optionalU64Value(w, len);
  });
}

export function op_read_fd_sync(rid, length, position) {
  const fd = smiU32(rid, "rid");
  const len = toU64(length, "length");
  const at = optionalU64(position, "position");
  return callService(SERVICE_OP.op_read_fd_sync, (w) => {
    w.u32(fd);
    w.u64(len);
    optionalU64Value(w, at);
  });
}

/// Fills the caller's view and answers the count, a Number (`#[number]`). The
/// Worker is blocked for the length of the call, as the isolate is for the
/// embedded op, so writing the view as the pieces arrive is what the op does.
export function op_read_fd_into_sync(rid, buf, position) {
  const fd = smiU32(rid, "rid");
  const view = fileBuffer(bytesOf(buf, "buf"));
  const at = optionalU64(position, "position");
  return readIntoSync(view, at, (length, pieceAt) =>
    callService(SERVICE_OP.op_read_fd_into_sync, (w) => {
      w.u32(fd);
      w.u64(length);
      optionalU64Value(w, pieceAt);
    }),
  );
}

export function op_read_compressed_file_sync(path) {
  const p = stringOf(path, "path");
  return callService(SERVICE_OP.op_read_compressed_file_sync, (w) => w.str(p));
}

/// `[size, digest]`, the serde tuple.
export function op_get_file_info_sync(path, algorithm) {
  const p = stringOf(path, "path");
  const a = stringOf(algorithm, "algorithm");
  return callService(SERVICE_OP.op_get_file_info_sync, (w) => {
    w.str(p);
    w.str(a);
  });
}

// ---- modules ------------------------------------------------------------------

/// A CommonJS module's source, found as Node finds it in the game's package.
/// The engine's `require` shim evaluates it; the host only resolves and reads.
export function op_require_resolve_and_read(specifier, referrerDir) {
  const name = stringOf(specifier, "specifier");
  const from = stringOf(referrerDir, "referrer_dir");
  return requiredModule(
    callService(SERVICE_OP.op_require_resolve_and_read, (w) => {
      w.str(name);
      w.str(from);
    }),
  );
}
