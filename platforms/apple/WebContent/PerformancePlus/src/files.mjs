// The file system's service calls: what their arguments look like on the
// service stream, and their answers as the embedded ops hand them to
// JavaScript.
//
// The host answers a structured value -- a stat, a saved-file list, zip
// entries -- as an array in the Rust struct's field order
// (engine/crates/core/src/runtime/service_fs.rs). The embedded op returns the
// struct through serde_v8, which makes an object with those fields as keys in
// that order; these rebuild exactly that object, so the engine's
// `02_file_manager.js` reads the same shape on both lanes.
//
// The arguments are converted by the lane before they reach here (op-args.mjs,
// checked against each op's Rust signature by the engine generator); this file
// only writes and reads values.

import { constructOpError } from "./engine-core.mjs";

/**
 * The largest single read the lanes ask the host for. A read into a larger
 * buffer is asked for in pieces of this size -- the same bytes in the same
 * order, because each piece continues where the last one ended -- so no one
 * answer approaches the synchronous reply ceiling or the host's outbox bound,
 * and the host never holds more than this at once for it.
 */
export const READ_CHUNK_BYTES = 64 * 1024 * 1024;

/** An `IOError`, the class every file-system failure has. */
export function ioError(message) {
  return constructOpError("IOError", message);
}

/**
 * A buffer file IO may use: backed by a fixed, unshared ArrayBuffer. The
 * embedded op refuses a shared or resizable one with this message, because a
 * write or read that raced content resizing or sharing it would tear.
 */
export function fileBuffer(view) {
  const store = view.buffer;
  if (Object.prototype.toString.call(store) !== "[object ArrayBuffer]" || store.resizable === true) {
    throw ioError("file IO requires a fixed, nonshared buffer");
  }
  return view;
}

/**
 * A write's data: the bytes when there are any, otherwise the string in its
 * encoding. The host takes the bytes first, as the op does, so the string is
 * not sent beside them.
 */
export function writeData(w, bytes, text, encoding) {
  if (bytes !== null) {
    w.bytes(bytes);
    w.null();
  } else {
    w.null();
    if (text === null) w.null();
    else w.str(text);
  }
  if (encoding === null) w.null();
  else w.str(encoding);
}

/** An `Option<u64>` argument: null, or the u64. */
export function optionalU64Value(w, value) {
  if (value === null) w.null();
  else w.u64(value);
}

/** `FileStat`: mode, size, atime, mtime, is_file, is_directory. */
export function fileStat([mode, size, atime, mtime, isFile, isDirectory]) {
  return { mode, size, atime, mtime, is_file: isFile, is_directory: isDirectory };
}

/** `StatResult`, untagged: a single stat's object, or `[{path, stat}]`. */
export function statResult([kind, payload]) {
  if (kind === 0) return fileStat(payload);
  return payload.map(([path, stat]) => ({ path, stat: fileStat(stat) }));
}

/** `Vec<SavedFileInfo>`: `{filePath, size, createTime}` each. */
export function savedFiles(list) {
  return list.map(([filePath, size, createTime]) => ({ filePath, size, createTime }));
}

/**
 * The op's zip entries: `{path, text?, bytes?, errMsg}`, the absent one of
 * `text` and `bytes` left out rather than null -- `skip_serializing_if`.
 */
export function zipEntries(list) {
  return list.map(([path, text, bytes, errMsg]) => {
    const entry = { path };
    if (text !== null) entry.text = text;
    if (bytes !== null) entry.bytes = bytes;
    entry.errMsg = errMsg;
    return entry;
  });
}

/** `require`'s module: `{code, abs_path, dir}`. */
export function requiredModule([code, absPath, dir]) {
  return { code, abs_path: absPath, dir };
}

/**
 * Read up to `view.byteLength` bytes from a descriptor into `view`, in pieces
 * of at most READ_CHUNK_BYTES, and answer how many arrived. `readPiece(length,
 * position)` asks the host for one piece at an absolute position (a BigInt) or
 * at the descriptor's cursor (null) and answers its bytes. Stops at the first
 * short piece, which is end of file -- where the embedded op's single read
 * stops too. Always asks at least once, as the op always seeks to a given
 * position even to read nothing.
 */
export function readIntoSync(view, position, readPiece) {
  let filled = 0;
  for (;;) {
    const wanted = Math.min(READ_CHUNK_BYTES, view.byteLength - filled);
    const bytes = readPiece(wanted, position === null ? null : position + BigInt(filled));
    view.set(bytes, filled);
    filled += bytes.byteLength;
    if (bytes.byteLength < wanted || filled >= view.byteLength) return filled;
  }
}

/**
 * The awaited form: the bytes that arrived, as a new Uint8Array the engine
 * copies into the caller's view when the promise settles -- the embedded op's
 * shape, which never writes the view while the read is in flight. A read that
 * fits one piece answers the host's bytes as they came.
 */
export function readIntoAsync(length, position, readPiece) {
  if (length <= READ_CHUNK_BYTES) return readPiece(length, position);
  const staged = new Uint8Array(length);
  let filled = 0;
  const next = () => {
    const wanted = Math.min(READ_CHUNK_BYTES, length - filled);
    return readPiece(wanted, position === null ? null : position + BigInt(filled)).then((bytes) => {
      staged.set(bytes, filled);
      filled += bytes.byteLength;
      return bytes.byteLength < wanted || filled >= length ? staged.subarray(0, filled) : next();
    });
  };
  return next();
}
