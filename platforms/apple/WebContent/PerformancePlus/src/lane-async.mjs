// Ops that return a promise the host settles.
//
// Most of them are service requests: the op's arguments are written in the
// shape its Rust signature takes, the service stream carries them (see
// `service.mjs`), and the host runs the same service code the embedded op
// calls -- `migo_services` -- so the answer and the error class are the ones
// every other platform gives.

import {
  arrayBufferOf,
  audioError,
  bufferInfo,
  bytesAsNumbers,
  checkEncodedAudioBytes,
  floatsAsNumbers,
  innerAudioState,
  transferOut,
} from "./audio.mjs";
import { engineHost } from "./engine-host.mjs";
import { fetchResponse } from "./network.mjs";
import { drained } from "./engine-frames.mjs";
import {
  fileBuffer,
  fileStat,
  optionalU64Value,
  readIntoAsync,
  savedFiles,
  statResult,
  writeData,
  zipEntries,
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
  toI32,
  toU64,
} from "./op-args.mjs";
import { SERVICE_OP } from "./service-ops.mjs";

/** The service stream, or a throw that says why there is none. */
export function servicesOf(host) {
  if (host.services === undefined) {
    const error = new Error("this producer has no service stream, and the op has no answer without one");
    error.name = "ServiceUnavailable";
    throw error;
  }
  return host.services;
}

const nothing = () => undefined;

/// The next frame-clock tick's timestamp, in milliseconds.
///
/// Waits first for any frame the window has not admitted, so a renderer that
/// is behind slows content to its own rate instead of accumulating frames; then
/// asks the host for a tick. Rejects with a RafError, as the Rust op does when
/// the renderer is gone, if the channel closes: the engine's frame loop ends on
/// it and says why.
export async function op_await_next_frame() {
  const host = engineHost();
  await drained();
  return new Promise((resolve, reject) => {
    const requested = host.session.requestFrame(host.runtimeGeneration, (timestampMillis) =>
      resolve(timestampMillis),
    );
    if (requested !== true) {
      const error = new Error("the frame channel is closed");
      error.name = "RafError";
      reject(error);
    }
  });
}

// ---- storage ---------------------------------------------------------------

export function op_storage_get_async(key) {
  const k = stringOf(key, "key");
  return servicesOf(engineHost()).request(SERVICE_OP.op_storage_get_async, (w) => w.str(k));
}

export function op_storage_set_async(key, value) {
  const k = stringOf(key, "key");
  const v = stringOf(value, "value");
  return servicesOf(engineHost())
    .request(SERVICE_OP.op_storage_set_async, (w) => {
      w.str(k);
      w.str(v);
    })
    .then(nothing);
}

export function op_storage_remove_async(key) {
  const k = stringOf(key, "key");
  return servicesOf(engineHost()).request(SERVICE_OP.op_storage_remove_async, (w) => w.str(k)).then(nothing);
}

export function op_storage_clear_async() {
  return servicesOf(engineHost()).request(SERVICE_OP.op_storage_clear_async).then(nothing);
}

export function op_storage_info_async() {
  return servicesOf(engineHost()).request(SERVICE_OP.op_storage_info_async);
}

// ---- images ------------------------------------------------------------------
//
// An image decodes on the host, where the texture it becomes lives: the path is
// resolved in the game's sandbox, the pixels go through the engine's own
// decoders and caches, and only the shared id and the size come back.

export function op_load_image(imageId, src, targetWidth, targetHeight) {
  const id = smiU32(imageId, "image_id");
  const source = stringOf(src, "src");
  const width = smiU32(targetWidth, "target_width");
  const height = smiU32(targetHeight, "target_height");
  return servicesOf(engineHost()).request(SERVICE_OP.op_load_image, (w) => {
    w.u32(id);
    w.str(source);
    w.u32(width);
    w.u32(height);
  });
}

export function op_load_image_subrect(imageId, src, sx, sy, sw, sh, resizeW, resizeH) {
  const args = [
    smiU32(imageId, "image_id"),
    stringOf(src, "src"),
    toI32(sx, "sx"),
    toI32(sy, "sy"),
    smiU32(sw, "sw"),
    smiU32(sh, "sh"),
    smiU32(resizeW, "resize_w"),
    smiU32(resizeH, "resize_h"),
  ];
  return servicesOf(engineHost()).request(SERVICE_OP.op_load_image_subrect, (w) => {
    w.u32(args[0]);
    w.str(args[1]);
    w.i32(args[2]);
    w.i32(args[3]);
    w.u32(args[4]);
    w.u32(args[5]);
    w.u32(args[6]);
    w.u32(args[7]);
  });
}

/// `#[serde] Vec<String>`: an array of strings, and nothing converted to one.
export function op_preload_images(paths) {
  if (!Array.isArray(paths) || !paths.every((path) => typeof path === "string")) {
    throw new TypeError("paths: expected an array of strings");
  }
  return servicesOf(engineHost()).request(SERVICE_OP.op_preload_images, (w) => {
    w.array(paths.length);
    for (const path of paths) w.str(path);
  });
}

// ---- files -------------------------------------------------------------------
//
// The sandboxed file system is the host's: each call is a request the host
// answers with the same `migo_services::fs` function the embedded op calls.
// Where the embedded op checks something before it returns its promise -- a
// buffer it cannot use -- this throws the same error before it asks.

const fs = () => servicesOf(engineHost());

export function op_access(path) {
  const p = stringOf(path, "path");
  return fs().request(SERVICE_OP.op_access, (w) => w.str(p));
}

export function op_write_or_append_file(path, dataBuf, dataStr, encoding, append, durable) {
  const p = stringOf(path, "path");
  const bytes = optionalBytesOf(dataBuf, "data_buf");
  const text = optionalStringOf(dataStr, "data_str");
  const enc = optionalStringOf(encoding, "encoding");
  const a = toBool(append, "append");
  const d = toBool(durable, "durable");
  if (bytes !== null) fileBuffer(bytes);
  return fs().request(SERVICE_OP.op_write_or_append_file, (w) => {
    w.str(p);
    writeData(w, bytes, text, enc);
    w.bool(a);
    w.bool(d);
  });
}

export function op_open_file(path, flag) {
  const p = stringOf(path, "path");
  const f = stringOf(flag, "flag");
  return fs().request(SERVICE_OP.op_open_file, (w) => {
    w.str(p);
    w.str(f);
  });
}

export function op_close_file(rid) {
  const fd = smiU32(rid, "rid");
  return fs().request(SERVICE_OP.op_close_file, (w) => w.u32(fd)).then(nothing);
}

export function op_copy_file(srcPath, destPath) {
  const src = stringOf(srcPath, "src_path");
  const dest = stringOf(destPath, "dest_path");
  return fs()
    .request(SERVICE_OP.op_copy_file, (w) => {
      w.str(src);
      w.str(dest);
    })
    .then(nothing);
}

export function op_fstat(rid) {
  const fd = smiU32(rid, "rid");
  return fs().request(SERVICE_OP.op_fstat, (w) => w.u32(fd)).then(fileStat);
}

export function op_ftruncate(rid, len) {
  const fd = smiU32(rid, "rid");
  const length = smiU64(len, "len");
  return fs()
    .request(SERVICE_OP.op_ftruncate, (w) => {
      w.u32(fd);
      w.u64(length);
    })
    .then(nothing);
}

export function op_mkdir(dirPath, recursive) {
  const p = stringOf(dirPath, "dir_path");
  const r = toBool(recursive, "recursive");
  return fs()
    .request(SERVICE_OP.op_mkdir, (w) => {
      w.str(p);
      w.bool(r);
    })
    .then(nothing);
}

export function op_readdir(dirPath) {
  const p = stringOf(dirPath, "dir_path");
  return fs().request(SERVICE_OP.op_readdir, (w) => w.str(p));
}

export function op_unlink(filePath) {
  const p = stringOf(filePath, "file_path");
  return fs().request(SERVICE_OP.op_unlink, (w) => w.str(p)).then(nothing);
}

export function op_rename(oldPath, newPath) {
  const from = stringOf(oldPath, "old_path");
  const to = stringOf(newPath, "new_path");
  return fs()
    .request(SERVICE_OP.op_rename, (w) => {
      w.str(from);
      w.str(to);
    })
    .then(nothing);
}

export function op_rmdir(dirPath, recursive) {
  const p = stringOf(dirPath, "dir_path");
  const r = toBool(recursive, "recursive");
  return fs()
    .request(SERVICE_OP.op_rmdir, (w) => {
      w.str(p);
      w.bool(r);
    })
    .then(nothing);
}

export function op_stat(path, recursive) {
  const p = stringOf(path, "path");
  const r = toBool(recursive, "recursive");
  return fs()
    .request(SERVICE_OP.op_stat, (w) => {
      w.str(p);
      w.bool(r);
    })
    .then(statResult);
}

/// Answers the count written as a BigInt, as the `#[bigint]` op does.
export function op_write_file(rid, dataBuf, dataStr, encoding, position) {
  const fd = smiU32(rid, "rid");
  const bytes = optionalBytesOf(dataBuf, "data_buf");
  const text = optionalStringOf(dataStr, "data_str");
  const enc = optionalStringOf(encoding, "encoding");
  const at = optionalU64(position, "position");
  if (bytes !== null) fileBuffer(bytes);
  return fs().request(SERVICE_OP.op_write_file, (w) => {
    w.u32(fd);
    writeData(w, bytes, text, enc);
    optionalU64Value(w, at);
  });
}

export function op_read_file(path, position, length) {
  const p = stringOf(path, "path");
  const at = optionalU64(position, "position");
  const len = optionalU64(length, "length");
  return fs().request(SERVICE_OP.op_read_file, (w) => {
    w.str(p);
    optionalU64Value(w, at);
    optionalU64Value(w, len);
  });
}

export function op_read_fd(rid, length, position) {
  const fd = smiU32(rid, "rid");
  const len = toU64(length, "length");
  const at = optionalU64(position, "position");
  return fs().request(SERVICE_OP.op_read_fd, (w) => {
    w.u32(fd);
    w.u64(len);
    optionalU64Value(w, at);
  });
}

/// The bytes read, as a new Uint8Array the engine's facade copies into the
/// caller's view -- the embedded op's shape. The view itself is only measured.
export function op_read_fd_into(rid, buf, position) {
  const fd = smiU32(rid, "rid");
  const view = fileBuffer(bytesOf(buf, "buf"));
  const at = optionalU64(position, "position");
  const services = fs();
  return readIntoAsync(view.byteLength, at, (length, pieceAt) =>
    services.request(SERVICE_OP.op_read_fd_into, (w) => {
      w.u32(fd);
      w.u64(length);
      optionalU64Value(w, pieceAt);
    }),
  );
}

export function op_read_compressed_file(path) {
  const p = stringOf(path, "path");
  return fs().request(SERVICE_OP.op_read_compressed_file, (w) => w.str(p));
}

export function op_read_zip_entry(zipPath, entriesJson) {
  const p = stringOf(zipPath, "zip_path");
  const entries = stringOf(entriesJson, "entries_json");
  return fs()
    .request(SERVICE_OP.op_read_zip_entry, (w) => {
      w.str(p);
      w.str(entries);
    })
    .then(zipEntries);
}

export function op_unzip(zipFilePath, targetPath) {
  const zip = stringOf(zipFilePath, "zip_file_path");
  const target = stringOf(targetPath, "target_path");
  return fs()
    .request(SERVICE_OP.op_unzip, (w) => {
      w.str(zip);
      w.str(target);
    })
    .then(nothing);
}

/// `[size, digest]`, the serde tuple.
export function op_get_file_info(path, algorithm) {
  const p = stringOf(path, "path");
  const a = stringOf(algorithm, "algorithm");
  return fs().request(SERVICE_OP.op_get_file_info, (w) => {
    w.str(p);
    w.str(a);
  });
}

export function op_list_saved_files(dir, prefix) {
  const d = stringOf(dir, "dir");
  const p = stringOf(prefix, "prefix");
  return fs()
    .request(SERVICE_OP.op_list_saved_files, (w) => {
      w.str(d);
      w.str(p);
    })
    .then(savedFiles);
}

// ---- audio ----------------------------------------------------------------------
//
// The audio ops that answer. Each command is sent when the op is called, in
// the order content made its calls, so "create the node, then stop it" holds
// across a request and a command; the promise only waits for the answer.

function audioRequest(op, writeArgs) {
  return servicesOf(engineHost()).request(op, writeArgs);
}

export function op_audio_close_context(ctxId) {
  const ctx = smiU32(ctxId, "ctx_id");
  return audioRequest(SERVICE_OP.op_audio_close_context, (w) => w.u32(ctx)).then(nothing);
}

export function op_audio_resume_context(ctxId) {
  const ctx = smiU32(ctxId, "ctx_id");
  return audioRequest(SERVICE_OP.op_audio_resume_context, (w) => w.u32(ctx)).then(nothing);
}

export function op_audio_suspend_context(ctxId) {
  const ctx = smiU32(ctxId, "ctx_id");
  return audioRequest(SERVICE_OP.op_audio_suspend_context, (w) => w.u32(ctx)).then(nothing);
}

/// `decodeAudioData`: the encoded bytes go to the host and the ArrayBuffer is
/// detached, as the embedded op detaches it before its promise exists. The
/// answer is the buffer's shape and the id of the host entry its PCM was
/// adopted as -- the PCM itself stays on the host.
///
/// The checks the embedded op makes before its future are made here too, in
/// its order, and throw where it throws: a buffer that cannot be given up, one
/// past the size bound (which would otherwise cross before the host refused
/// it).
export function op_audio_decode_audio_data(ctxId, data) {
  const ctx = smiU32(ctxId, "ctx_id");
  const buffer = arrayBufferOf(data);
  const cannotDetach = "audioData is detached or cannot be detached";
  if (buffer.detached === true) throw audioError(cannotDetach);
  checkEncodedAudioBytes(buffer.byteLength);
  const encoded = transferOut(buffer, cannotDetach);
  return audioRequest(SERVICE_OP.op_audio_decode_audio_data, (w) => {
    w.u32(ctx);
    w.bytes(encoded);
  }).then(bufferInfo);
}

export function op_audio_stop(nodeId, when) {
  const node = smiU32(nodeId, "node_id");
  const at = toF64(when, "when");
  return audioRequest(SERVICE_OP.op_audio_stop, (w) => {
    w.u32(node);
    w.f64(at);
  }).then(nothing);
}

export function op_audio_connect(src, dst, srcOutput, dstInput) {
  const from = smiU32(src, "src");
  const to = smiU32(dst, "dst");
  const output = smiU32(srcOutput, "src_output");
  const input = smiU32(dstInput, "dst_input");
  return audioRequest(SERVICE_OP.op_audio_connect, (w) => {
    w.u32(from);
    w.u32(to);
    w.u32(output);
    w.u32(input);
  }).then(nothing);
}

export function op_audio_disconnect(nodeId) {
  const node = smiU32(nodeId, "node_id");
  return audioRequest(SERVICE_OP.op_audio_disconnect, (w) => w.u32(node)).then(nothing);
}

/// `#[buffer] Vec<u8>`: a Uint8Array, as the host sent it.
export function op_audio_analyser_byte_time_domain(nodeId) {
  const node = smiU32(nodeId, "node_id");
  return audioRequest(SERVICE_OP.op_audio_analyser_byte_time_domain, (w) => w.u32(node));
}

/// `#[buffer] Vec<u8>` holding `f32`s: the facade views them as a Float32Array.
export function op_audio_analyser_float_time_domain(nodeId) {
  const node = smiU32(nodeId, "node_id");
  return audioRequest(SERVICE_OP.op_audio_analyser_float_time_domain, (w) => w.u32(node));
}

/// `#[serde] Vec<u8>`: an Array of Numbers.
export function op_audio_analyser_byte_frequency(nodeId) {
  const node = smiU32(nodeId, "node_id");
  return audioRequest(SERVICE_OP.op_audio_analyser_byte_frequency, (w) => w.u32(node)).then(
    bytesAsNumbers,
  );
}

/// `#[serde] Vec<f32>`: an Array of Numbers.
export function op_audio_analyser_float_frequency(nodeId) {
  const node = smiU32(nodeId, "node_id");
  return audioRequest(SERVICE_OP.op_audio_analyser_float_frequency, (w) => w.u32(node)).then(
    floatsAsNumbers,
  );
}

/// `#[serde] (Vec<f32>, Vec<f32>)`: magnitude and phase, two Arrays.
export function op_audio_get_frequency_response(nodeId, frequencies) {
  const node = smiU32(nodeId, "node_id");
  const hz = bytesOf(frequencies, "frequencies");
  return audioRequest(SERVICE_OP.op_audio_get_frequency_response, (w) => {
    w.u32(node);
    w.bytes(hz);
  }).then(([magnitude, phase]) => [floatsAsNumbers(magnitude), floatsAsNumbers(phase)]);
}

export function op_audio_get_reduction(nodeId) {
  const node = smiU32(nodeId, "node_id");
  return audioRequest(SERVICE_OP.op_audio_get_reduction, (w) => w.u32(node));
}

/// An InnerAudioContext's `src`: a sound in the game's package, read and
/// decoded on the host. A streamed `http(s)` source is refused until this lane
/// has its network service.
export function op_inner_audio_load_url(id, src) {
  const inner = smiU32(id, "id");
  const source = stringOf(src, "src");
  return audioRequest(SERVICE_OP.op_inner_audio_load_url, (w) => {
    w.u32(inner);
    w.str(source);
  }).then(nothing);
}

export function op_inner_audio_get_state(id) {
  const inner = smiU32(id, "id");
  return audioRequest(SERVICE_OP.op_inner_audio_get_state, (w) => w.u32(inner)).then(innerAudioState);
}

// ---- network ------------------------------------------------------------------

/// Send the request `rid` names and answer its head, with the id its body is
/// read from (`core.read`).
///
/// The request is consumed by the send, as it is in the embedded runtime: a
/// second send on the same id finds nothing.
export function op_fetch_send(rid) {
  const request = smiU32(rid, "rid");
  return servicesOf(engineHost())
    .request(SERVICE_OP.op_fetch_send, (w) => w.u32(request))
    .then(fetchResponse);
}
