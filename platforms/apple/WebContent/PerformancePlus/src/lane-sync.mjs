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
import { recordProducerError } from "./lane-local.mjs";
import { takeSnapshotSize } from "./lane-stream.mjs";
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
  optionalSmiU32,
  optionalStringOf,
  optionalU64,
  smiU32,
  smiU64,
  stringOf,
  toBool,
  toF64,
  toI32,
  toU32,
  toU64,
} from "./op-args.mjs";
import { arrayBufferAnswer } from "./audio.mjs";
import { byteStringOf, fetchHandles, udpBound, writeHeaders } from "./network.mjs";
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
  READ_PIXELS_LAYOUT_BYTES,
  SYNC_ERROR_OPERATION_FAILED,
  SYNC_OP_CANVAS2D_IMAGE_DATA,
  SYNC_OP_CANVAS2D_SNAPSHOT,
  SYNC_OP_READ_PIXELS,
  encodeCanvas2DPixelsParams,
  decodeReadPixelsLayout,
  encodeReadPixelsParams,
  readPixelsReplyBytes,
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

// ---- audio ----------------------------------------------------------------------
//
// Audio plays on the host. These are the audio ops whose failure content
// catches where it made the call -- creating a context or a player, admitting
// an AudioBuffer, setting the session's audio option -- so they wait for the
// host's answer; everything else about audio is a command (lane-command.mjs).

/// `new AudioContext()`: the id is content's, so the context is usable at once.
export function op_audio_create_context(ctxId, sampleRate) {
  const ctx = smiU32(ctxId, "ctx_id");
  const rate = smiU32(sampleRate, "sample_rate");
  callService(SERVICE_OP.op_audio_create_context, (w) => {
    w.u32(ctx);
    w.u32(rate);
  });
}

/// Admit a writable AudioBuffer before content allocates its backing.
export function op_audio_reserve_buffer(channels, length, sampleRate) {
  const c = smiU32(channels, "channels");
  const l = smiU32(length, "length");
  const r = smiU32(sampleRate, "sample_rate");
  return callService(SERVICE_OP.op_audio_reserve_buffer, (w) => {
    w.u32(c);
    w.u32(l);
    w.u32(r);
  });
}

/// A decoded buffer's channels, when content first reads them: the one time
/// its PCM crosses to WebContent. The planar ArrayBuffer is the reply's own.
export function op_audio_materialize_buffer(bufferId) {
  const id = smiU32(bufferId, "buffer_id");
  return arrayBufferAnswer(callService(SERVICE_OP.op_audio_materialize_buffer, (w) => w.u32(id)));
}

/// `setInnerAudioOption`, whose refusal is the facade's `fail`: answered, so
/// it is not reported as success before the host has said whether it can.
export function op_audio_set_inner_audio_option(mixWithOther, obeyMuteSwitch, speakerOn) {
  const mix = toBool(mixWithOther, "mix_with_other");
  const obey = toBool(obeyMuteSwitch, "obey_mute_switch");
  const speaker = toBool(speakerOn, "speaker_on");
  callService(SERVICE_OP.op_audio_set_inner_audio_option, (w) => {
    w.bool(mix);
    w.bool(obey);
    w.bool(speaker);
  });
}

export function op_audio_get_available_audio_sources() {
  return callService(SERVICE_OP.op_audio_get_available_audio_sources);
}

export function op_media_audio_player_create(playerId) {
  const id = smiU32(playerId, "player_id");
  callService(SERVICE_OP.op_media_audio_player_create, (w) => w.u32(id));
}

/// `createInnerAudioContext()`: the id is content's, as for a context.
export function op_inner_audio_create(id) {
  const inner = smiU32(id, "id");
  callService(SERVICE_OP.op_inner_audio_create, (w) => w.u32(inner));
}

// ---- network ------------------------------------------------------------------

/// Build a request and answer its handles. Nothing is sent yet: the send is
/// `op_fetch_send`, which is awaited.
///
/// Synchronous because the engine's `fetch` reports a refusal by throwing here
/// -- a host the policy does not allow, a URL that is not one -- before there
/// is a promise for it to reject. The work is the host's
/// (`migo_services::network::fetch`), so the refusal is the same one every
/// other platform gives.
///
/// `method` and `headers` are `#[serde] ByteString`, which the conversion gate
/// exempts; network.mjs restates serde_v8's rule for them.
export function op_fetch(
  method,
  url,
  headers,
  clientRid,
  hasBody,
  data,
  resource,
  timeout,
  enableHttp2,
  enableCache,
) {
  const verb = byteStringOf(method);
  const target = stringOf(url, "url");
  const client = optionalSmiU32(clientRid, "client_rid");
  const withBody = toBool(hasBody, "has_body");
  const body = optionalBytesOf(data, "data");
  const bodyResource = optionalSmiU32(resource, "resource");
  const deadline = smiU32(timeout, "timeout");
  const http2 = toBool(enableHttp2, "enable_http2");
  const cache = toBool(enableCache, "enable_cache");
  return fetchHandles(
    callService(SERVICE_OP.op_fetch, (w) => {
      w.bytes(verb);
      w.str(target);
      writeHeaders(w, headers);
      if (client === null) w.null();
      else w.u32(client);
      w.bool(withBody);
      if (body === null) w.null();
      else w.bytes(body);
      if (bodyResource === null) w.null();
      else w.u32(bodyResource);
      w.u32(deadline);
      w.bool(http2);
      w.bool(cache);
    }),
  );
}

/// Bind a UDP socket. Synchronous, as the facade's `bind` is: a port already
/// taken is a throw content catches rather than a promise it never awaited.
export function op_udp_bind(port, socketType) {
  const local = smiU32(port, "port");
  const kind = stringOf(socketType, "socket_type");
  return udpBound(
    callService(SERVICE_OP.op_udp_bind, (w) => {
      w.u32(local);
      w.str(kind);
    }),
  );
}

/// The handle an in-flight `uploadFile` is aborted through.
///
/// Synchronous because it names a host resource -- the upload streams a file
/// the host opened -- and content holds it before it awaits the upload, so
/// `abort()` can close it at any point after that.
export function op_fetch_upload_cancel_handle() {
  return callService(SERVICE_OP.op_fetch_upload_cancel_handle);
}

// ---- subpackages -----------------------------------------------------------------
//
// What a game asks before it loads one: is it here already, what is covering it,
// and has the mount view changed since it last resolved a path. The host reads
// its own mount table and the game's package store, which is where the answer
// is on every platform.

export function op_get_sub_packages() {
  return callService(SERVICE_OP.op_get_sub_packages);
}

export function op_get_mount_generation() {
  return callService(SERVICE_OP.op_get_mount_generation);
}

export function op_get_subpackage_identity(root) {
  const path = stringOf(root, "root");
  return callService(SERVICE_OP.op_get_subpackage_identity, (w) => w.str(path));
}

export function op_is_subpackage_installed(root) {
  const path = stringOf(root, "root");
  return callService(SERVICE_OP.op_is_subpackage_installed, (w) => w.str(path));
}

export function op_is_subpackage_persisted(name, root) {
  const which = stringOf(name, "name");
  const path = stringOf(root, "root");
  return callService(SERVICE_OP.op_is_subpackage_persisted, (w) => {
    w.str(which);
    w.str(path);
  });
}

export function op_get_workers_path() {
  return callService(SERVICE_OP.op_get_workers_path);
}

// ---- readPixels -------------------------------------------------------------
//
// The one query whose answer is pixels rather than a number, and the one whose
// answer has to be placed rather than returned: `readPixels` writes into a view
// the caller already holds, at positions the renderer's `PACK_*` state decides.
//
// The split between the two sides is what the ops' own comments state. This
// side owns the view -- its kind, its length, the caller's offset into it -- and
// nothing here can see `PACK_*`, because the engine's encoder writes
// `pixelStorei` straight into the command stream this producer forwards unread.
// The host owns the state and the framebuffer, and answers with the layout in
// front of the rows (`frame_wire::sync::ReadPixelsLayout`).
//
// TWO DIFFERENCES FROM THE IN-PROCESS OP, both stated rather than hidden:
//
//   * The pair. This lane's synchronous operation carries `RGBA`/`UNSIGNED_BYTE`,
//     which is the pair WebGL 1 guarantees for every framebuffer and what
//     content overwhelmingly reads. Another pair is refused here with
//     `INVALID_OPERATION` -- which is what the specification says for a pair an
//     implementation does not offer -- where the embedded runtime would ask the
//     renderer and might answer it.
//   * When the destination is measured. In process the renderer refuses a
//     footprint that overruns the view before the read; here the footprint is
//     not known until the layout arrives, so the refusal is after. Content sees
//     the same error and the same untouched view either way: the difference is
//     a readback the host did and this side dropped, in a case that is a
//     content bug.

/// WebGL error codes, as `error_state::codes` names them.
const GL_INVALID_ENUM = 0x0500;
const GL_INVALID_VALUE = 0x0501;
const GL_INVALID_OPERATION = 0x0502;
const GL_OUT_OF_MEMORY = 0x0505;

/// The pair the host's synchronous readback carries.
const READ_PIXELS_FORMAT = 0x1908;
const READ_PIXELS_TYPE = 0x1401;

/// `shared::protocol::render_cmd::MAX_SYNC_READBACK_BYTES`.
const MAX_SYNC_READBACK_BYTES = 64 * 1024 * 1024;

/**
 * `webgl_readback_bytes_per_pixel`: the byte width of a recognised GL pixel
 * representation, or null for an enum with no inferred width.
 *
 * A storage-size calculation, not validation -- a known representation need not
 * be a legal `readPixels` pair, which is what the refusal below is for.
 */
function readbackBytesPerPixel(format, type) {
  let components;
  switch (format) {
    case 0x1908: case 0x8D99: case 0x80E1: components = 4; break;
    case 0x1907: case 0x8D98: components = 3; break;
    case 0x8227: case 0x8228: case 0x190A: case 0x84F9: components = 2; break;
    case 0x1903: case 0x8D94: case 0x1909: case 0x1906: case 0x1902: case 0x1901:
      components = 1; break;
    default: return null;
  }
  switch (type) {
    case 0x1400: case 0x1401: return components;
    case 0x1402: case 0x1403: case 0x140B: case 0x8D61: return components * 2;
    case 0x1404: case 0x1405: case 0x1406: return components * 4;
    case 0x8363: case 0x8033: case 0x8034: case 0x8365: case 0x8366: return 2;
    case 0x8368: case 0x8C3B: case 0x8C3E: case 0x84FA: return 4;
    case 0x8DAD: return 8;
    default: return null;
  }
}

/**
 * `read_pixels_view_layout`: the destination's byte length and element size, or
 * null when the view is not one this pixel type may be read into.
 *
 * The type decides the kind: `UNSIGNED_BYTE` into a `Uint8Array` or a clamped
 * one, the packed shorts into a `Uint16Array`, and so on. A view of another kind
 * is the specification's `INVALID_OPERATION`, and no view at all its
 * `INVALID_VALUE` -- which is why this answers null for both and the caller
 * tells them apart.
 */
function readPixelsViewLayout(pixels, type) {
  const is = (constructor) => pixels instanceof constructor;
  let elementBytes;
  switch (type) {
    case 0x1400: elementBytes = is(Int8Array) ? 1 : 0; break;
    case 0x1401: elementBytes = is(Uint8Array) || is(Uint8ClampedArray) ? 1 : 0; break;
    case 0x1402: elementBytes = is(Int16Array) ? 2 : 0; break;
    case 0x1403: case 0x140B: case 0x8D61: case 0x8363: case 0x8033: case 0x8034:
    case 0x8365: case 0x8366:
      elementBytes = is(Uint16Array) ? 2 : 0; break;
    case 0x1404: elementBytes = is(Int32Array) ? 4 : 0; break;
    case 0x1405: case 0x8368: case 0x8C3B: case 0x8C3E: case 0x84FA: case 0x8DAD:
      elementBytes = is(Uint32Array) ? 4 : 0; break;
    case 0x1406: elementBytes = is(Float32Array) ? 4 : 0; break;
    default: return null;
  }
  if (elementBytes === 0 || !ArrayBuffer.isView(pixels)) return null;
  return { byteLength: pixels.byteLength, elementBytes };
}

/**
 * `read_pixels_element_offset`: WebIDL's unsigned long long after ToNumber.
 *
 * A negative offset wraps rather than rounding to zero, so the bounds check
 * below refuses it instead of reading from the start of the view.
 */
function readPixelsElementOffset(value) {
  const MODULUS = 18446744073709551616;
  if (!Number.isFinite(value)) return 0;
  if (value >= 0 && value < MODULUS) return value;
  const remainder = Math.trunc(value) % MODULUS;
  return remainder < 0 ? MODULUS + remainder : remainder;
}

/**
 * `readPixels(x, y, width, height, format, type, view, dstOffset)`.
 *
 * Answers `{data, firstByte, rowBytes, rowStride, height}` -- the rows and where
 * they go -- which is what the engine's facade copies into the caller's view, or
 * null when the call was refused, with the error recorded for `getError` exactly
 * as the op records it.
 */
export function op_read_pixels(canvasId, x, y, width, height, format, type_, pixels, dst_offset) {
  const canvas = smiU32(canvasId, "canvas_id");
  const left = toI32(x, "x");
  const bottom = toI32(y, "y");
  const columns = toI32(width, "width");
  const rows = toI32(height, "height");
  const glFormat = smiU32(format, "format");
  const glType = smiU32(type_, "type_");
  const offsetElements = toF64(dst_offset, "dst_offset");

  const bytesPerPixel = readbackBytesPerPixel(glFormat, glType);
  if (bytesPerPixel === null) {
    recordProducerError(canvas, GL_INVALID_ENUM);
    return null;
  }
  if (columns < 0 || rows < 0) {
    recordProducerError(canvas, GL_INVALID_VALUE);
    return null;
  }
  const pixelBytes = columns * rows * bytesPerPixel;
  if (!Number.isSafeInteger(pixelBytes) || pixelBytes > MAX_SYNC_READBACK_BYTES) {
    recordProducerError(canvas, GL_OUT_OF_MEMORY);
    return null;
  }

  const view = readPixelsViewLayout(pixels, glType);
  if (view === null) {
    recordProducerError(
      canvas,
      pixels === null || pixels === undefined ? GL_INVALID_VALUE : GL_INVALID_OPERATION,
    );
    return null;
  }
  const destinationByteOffset = readPixelsElementOffset(offsetElements) * view.elementBytes;
  if (!Number.isSafeInteger(destinationByteOffset) || destinationByteOffset > view.byteLength) {
    recordProducerError(canvas, GL_INVALID_OPERATION);
    return null;
  }
  if (pixelBytes > view.byteLength - destinationByteOffset) {
    recordProducerError(canvas, GL_INVALID_OPERATION);
    return null;
  }

  // An empty rectangle reads nothing. In process the renderer answers it with a
  // canonical empty layout and the facade returns without touching the view;
  // answering null here is the same nothing, and it keeps a request the host
  // refuses off the wire.
  if (columns === 0 || rows === 0) return null;

  if (glFormat !== READ_PIXELS_FORMAT || glType !== READ_PIXELS_TYPE) {
    recordProducerError(canvas, GL_INVALID_OPERATION);
    return null;
  }

  let reply;
  try {
    reply = ask(
      SYNC_OP_READ_PIXELS,
      readPixelsReplyBytes(columns, rows),
      encodeReadPixelsParams({
        canvasId: canvas,
        x: left,
        y: bottom,
        width: columns,
        height: rows,
        format: glFormat,
        type: glType,
      }),
    );
  } catch (error) {
    // The host tried the read and it failed -- a canvas that is gone, an
    // incomplete framebuffer, a GL error. That is the op's `INVALID_OPERATION`
    // rather than an exception: `readPixels` does not throw. Anything else (the
    // deadline, the session ending) is not a WebGL outcome and is left to
    // propagate, as every other synchronous query on this lane leaves it.
    if (error && error.code === SYNC_ERROR_OPERATION_FAILED) {
      recordProducerError(canvas, GL_INVALID_OPERATION);
      return null;
    }
    throw error;
  }

  const layout = decodeReadPixelsLayout(reply.subarray(0, READ_PIXELS_LAYOUT_BYTES));
  const data = reply.subarray(READ_PIXELS_LAYOUT_BYTES);
  // The footprint the host's PACK state puts in this view: the skips, then a
  // stride per row but only the pixels of the last one.
  const footprint =
    layout.height === 0
      ? 0
      : layout.firstByte + (layout.height - 1) * layout.rowStride + layout.rowBytes;
  if (destinationByteOffset + footprint > view.byteLength) {
    recordProducerError(canvas, GL_INVALID_OPERATION);
    return null;
  }
  return {
    data,
    firstByte: destinationByteOffset + layout.firstByte,
    rowBytes: layout.rowBytes,
    rowStride: layout.rowStride,
    height: layout.height,
  };
}

// ---- Canvas2D pixels --------------------------------------------------------
//
// Two reads, one shape. `getImageData` in the engine's own facade captures into
// the host's snapshot pool and asks for the bytes only when content reads them,
// so the snapshot read is the common one; the direct rectangle read is the
// fallback the facade takes for a read it cannot capture (zero area, out of
// bounds). Both answer with tightly packed RGBA8 -- no layout travels with them,
// unlike `readPixels`, because nothing but the rectangle decides where 2D rows
// go.
//
// Both ops answer the empty array where the in-process op answers an empty
// `Vec`: `getImageData` has no way to report a failure to content, and a
// zero-filled picture would be a blank label nobody can see the cause of. The
// host logs, as the op logs.

/// `shared::protocol::render_cmd::checked_canvas_rgba_byte_len`'s two bounds.
const MAX_CANVAS2D_DIMENSION = 8192;
const MAX_CANVAS2D_PIXELS = MAX_CANVAS2D_DIMENSION * MAX_CANVAS2D_DIMENSION;

const EMPTY_PIXELS = new Uint8Array(0);

function canvas2dPixelBytes(width, height) {
  if (width === 0 || height === 0) return 0;
  if (width > MAX_CANVAS2D_DIMENSION || height > MAX_CANVAS2D_DIMENSION) return null;
  const pixels = width * height;
  return pixels <= MAX_CANVAS2D_PIXELS ? pixels * 4 : null;
}

/** Ask for one of the two reads, or answer empty the way the op does. */
function canvas2dPixels(operation, params, replyBytes) {
  try {
    return ask(operation, replyBytes, params);
  } catch (error) {
    // The host tried the read and could not do it: a canvas that is gone, a
    // snapshot the pool no longer holds. The op answers empty for those.
    if (error && error.code === SYNC_ERROR_OPERATION_FAILED) return EMPTY_PIXELS;
    throw error;
  }
}

/** `getImageData(x, y, w, h)`, for a read the facade did not capture. */
export function op_get_image_data(canvasId, x, y, width, height) {
  const canvas = smiU32(canvasId, "canvas_id");
  const left = toI32(x, "x");
  const top = toI32(y, "y");
  const w = toU32(width, "width");
  const h = toU32(height, "height");
  const bytes = canvas2dPixelBytes(w, h);
  if (bytes === null || bytes === 0) return EMPTY_PIXELS;
  return canvas2dPixels(
    SYNC_OP_CANVAS2D_IMAGE_DATA,
    encodeCanvas2DPixelsParams({ target: canvas, x: left, y: top, width: w, height: h }),
    bytes,
  );
}

/**
 * The pixels of a snapshot this producer captured earlier in the frame.
 *
 * The size comes from the capture: the op takes only an id because in process
 * the renderer's pool knows the rest, and here the reservation has to be made
 * before the question is asked. A snapshot this producer did not capture, or one
 * whose frame has ended, is the empty answer the op gives for a pool miss.
 */
export function op_force_readback_snapshot(snapshotId) {
  const id = smiU32(snapshotId, "snapshot_id");
  if (id === 0) return EMPTY_PIXELS;
  const size = takeSnapshotSize(id);
  if (size === undefined) return EMPTY_PIXELS;
  const [width, height] = size;
  const bytes = canvas2dPixelBytes(width, height);
  if (bytes === null || bytes === 0) return EMPTY_PIXELS;
  return canvas2dPixels(
    SYNC_OP_CANVAS2D_SNAPSHOT,
    encodeCanvas2DPixelsParams({ target: id, x: 0, y: 0, width, height }),
    bytes,
  );
}
