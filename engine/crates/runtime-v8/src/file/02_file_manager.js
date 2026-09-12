import {
  op_access, op_access_sync,
  op_write_or_append_file, op_write_or_append_file_sync,
  op_open_file, op_open_file_sync,
  op_close_file, op_close_file_sync,
  op_copy_file, op_copy_file_sync,
  op_fstat, op_fstat_sync,
  op_ftruncate, op_ftruncate_sync,
  op_mkdir, op_mkdir_sync,
  op_readdir, op_readdir_sync,
  op_unlink, op_unlink_sync,
  op_rename, op_rename_sync,
  op_rmdir, op_rmdir_sync,
  op_stat, op_stat_sync,
  op_write_file, op_write_file_sync,
  op_read_compressed_file, op_read_compressed_file_sync,
  op_read_fd, op_read_fd_sync,
  op_read_fd_into, op_read_fd_into_sync,
  op_read_file, op_read_file_sync,
  op_read_zip_entry,
  op_unzip,
  op_get_file_info, op_get_file_info_sync,
  op_list_saved_files,
  op_decode_multi_formats
} from "ext:core/ops";

import { core, primordials } from "ext:core/mod.js";
import { wrapAsync } from "ext:host_v8_base/02_async.js";
import { FileStats, Stats } from "./02_file_stats.js";

const {
  Error,
  ArrayBuffer,
  ArrayBufferPrototypeGetByteLength,
  Uint8Array,
  TypedArrayPrototypeGetByteLength,
  TypedArrayPrototypeSet,
} = primordials;

class IOError extends Error {
  constructor(msg) {
    super(msg);
    this.name = "IOError";
  }
}
core.registerErrorClass("IOError", IOError);

function wrapSync(fn, failPrefix) {
  try {
    return fn();
  } catch (err) {
    const msg = err?.message || String(err);
    throw { errMsg: `${failPrefix}:fail ${msg}`, message: `${failPrefix}:fail ${msg}` };
  }
}

/**
 * Shape the `op_read_zip_entry` list into the `{entries: {path: {data, errMsg}}}`
 * object callers see.
 *
 * `bytes` arrives as a Uint8Array over a buffer sized exactly to the entry, so
 * `.buffer` is the caller's ArrayBuffer with no copy. It used to arrive as a
 * base64 string that this function decoded one byte at a time.
 *
 * A module-level function rather than an inline `.then` body so it can be
 * driven directly from a test without a zip file, a VFS and a live op.
 */
function zipEntriesFromOpResult(list) {
  const result = { entries: {} };
  for (let i = 0; i < list.length; i++) {
    const item = list[i];
    let data = null;
    if (item.bytes !== undefined) {
      data = item.bytes.buffer;
    } else if (item.text !== undefined) {
      data = item.text;
    }
    result.entries[item.path] = { data, errMsg: item.errMsg };
  }
  return result;
}

function toUint8Array(data, offset = 0, length) {
  if (typeof data === "string" || data instanceof String) {
    return { data_str: String(data), data_buf: null };
  }
  if (data instanceof ArrayBuffer || ArrayBuffer.isView(data)) {
    const buf = data instanceof ArrayBuffer
      ? new Uint8Array(data)
      : new Uint8Array(data.buffer, data.byteOffset, data.byteLength);

    const start = offset >>> 0;
    const end = length !== undefined ? Math.min(start + (length >>> 0), buf.length) : buf.length;
    return { data_buf: buf.subarray(start, end), data_str: null };
  }
  throw new IOError("write: invalid data type (expected string or ArrayBuffer/TypedArray)");
}

function ensureFd(fd) {
  const n = Number(fd);
  if (!Number.isFinite(n)) throw new IOError("invalid fd");
  return n;
}

// Pre-computed hex lookup table (avoids toString(16) + padStart per byte)
const HEX_LUT = new Array(256);
for (let i = 0; i < 256; i++) HEX_LUT[i] = (i < 16 ? "0" : "") + i.toString(16);

const SAVE_FILE_DIR = "/user";
let _saveFileCounter = 0;
const SAVED_FILE_REGISTRY = `${SAVE_FILE_DIR}/.migo_saved_files.json`;
const _trackedSavedFilePaths = new Set();
let _savedRegistryOp = Promise.resolve();

function getPathExtension(path) {
  if (typeof path !== "string" || path.length === 0) {
    return "";
  }
  const slash = path.lastIndexOf("/");
  const dot = path.lastIndexOf(".");
  if (dot <= slash || dot === path.length - 1) {
    return "";
  }
  return path.slice(dot);
}

function makeSavedFilePath(tempFilePath) {
  const ts = Date.now().toString(36);
  const seq = (++_saveFileCounter).toString(36);
  const ext = getPathExtension(tempFilePath);
  return `${SAVE_FILE_DIR}/saved_${ts}_${seq}${ext}`;
}

function resolveSavedFilePath(filePath, tempFilePath) {
  if (typeof filePath === "string" && filePath.length > 0) {
    if (filePath !== SAVE_FILE_DIR && filePath !== SAVED_FILE_REGISTRY && filePath.startsWith(`${SAVE_FILE_DIR}/`)) {
      return filePath;
    }
    throw new IOError("filePath must be a file path under /user");
  }
  return makeSavedFilePath(tempFilePath);
}

function queueSavedRegistryOp(op) {
  const run = _savedRegistryOp.then(op, op);
  _savedRegistryOp = run.then(() => undefined, () => undefined);
  return run;
}

function rememberSavedFilePath(path) {
  if (typeof path === "string" && path.startsWith(`${SAVE_FILE_DIR}/`)) {
    _trackedSavedFilePaths.add(path);
  }
}

function shouldTrackSavedFilePath(path) {
  if (typeof path !== "string") return false;
  if (!path.startsWith(`${SAVE_FILE_DIR}/`)) return false;
  return !/^\/user\/saved_[^/]+$/.test(path);
}

function decodeUtf8Bytes(data) {
  const bytes = data instanceof Uint8Array ? data : new Uint8Array(data.buffer || data);
  return op_decode_multi_formats(bytes, 'utf8');
}

async function loadSavedFileRegistry() {
  try {
    const raw = await op_read_file(SAVED_FILE_REGISTRY, undefined, undefined);
    return Array.from(new Set(
      decodeUtf8Bytes(raw)
        .split("\n")
        .map((s) => s.trim())
        .filter((s) => s.length > 0)
    ));
  } catch (_) {
    return [];
  }
}

function loadSavedFileRegistrySync() {
  try {
    const raw = op_read_file_sync(SAVED_FILE_REGISTRY, undefined, undefined);
    return Array.from(new Set(
      decodeUtf8Bytes(raw)
        .split("\n")
        .map((s) => s.trim())
        .filter((s) => s.length > 0)
    ));
  } catch (_) {
    return [];
  }
}

async function persistSavedFileRegistry(paths) {
  await op_write_or_append_file(SAVED_FILE_REGISTRY, null, `${paths.join("\n")}\n`, "utf8", false, true);
}

function persistSavedFileRegistrySync(paths) {
  op_write_or_append_file_sync(SAVED_FILE_REGISTRY, null, `${paths.join("\n")}\n`, "utf8", false, true);
}

async function trackSavedFilePath(path) {
  rememberSavedFilePath(path);
  if (!shouldTrackSavedFilePath(path)) return;
  await queueSavedRegistryOp(async () => {
    const current = await loadSavedFileRegistry();
    if (!current.includes(path)) {
      await op_write_or_append_file(SAVED_FILE_REGISTRY, null, `${path}\n`, "utf8", true, true);
    }
  });
}

function trackSavedFilePathSync(path) {
  rememberSavedFilePath(path);
  if (!shouldTrackSavedFilePath(path)) return;
    op_write_or_append_file_sync(SAVED_FILE_REGISTRY, null, `${path}\n`, "utf8", true, true);
}

async function untrackSavedFilePath(path) {
  _trackedSavedFilePaths.delete(path);
  if (!shouldTrackSavedFilePath(path)) return;
  await queueSavedRegistryOp(async () => {
    const current = await loadSavedFileRegistry();
    const next = current.filter((p) => p !== path);
    await persistSavedFileRegistry(next);
  });
}

class BaseFileManager {
  //
  // access
  //
  static access(options) {
    return wrapAsync('access', () => {
      return op_access(options.path).then((ok) => {
        if (!ok) throw new IOError("No such file or directory");
      });
    }, options);
  }

  static accessSync(path_or_obj) {
    const path = typeof path_or_obj === "object" && path_or_obj !== null ? path_or_obj.path : path_or_obj;
    wrapSync(() => {
      if (!op_access_sync(path)) {
        throw new IOError("No such file or directory");
      }
    }, "accessSync");
  }

  //
  // writeFile / appendFile (path)
  //
  static writeFile(options) {
    return BaseFileManager.#writeFileCommon(options, false);
  }

  static appendFile(options) {
    return BaseFileManager.#writeFileCommon(options, true);
  }

  static writeFileSync(filePath_or_obj, data_arg, encoding_arg) {
    let filePath, data, encoding, durable = true;
    if (typeof filePath_or_obj === "object" && filePath_or_obj !== null && !ArrayBuffer.isView(filePath_or_obj) && !(filePath_or_obj instanceof ArrayBuffer)) {
      ({ filePath, data, encoding = "utf8" } = filePath_or_obj);
      // Opt-in fast (non-crash-safe) write; defaults to durable.
      durable = filePath_or_obj.durable !== false;
    } else {
      filePath = filePath_or_obj;
      data = data_arg;
      encoding = encoding_arg || "utf8";
    }
    const { data_buf, data_str } = toUint8Array(data);
    const eff = BaseFileManager.#effectiveDurable(filePath, durable);
    wrapSync(() => {
      const ok = op_write_or_append_file_sync(filePath, data_buf, data_str, encoding, false, eff);
      if (!ok) throw new IOError("unknown error");
      return undefined;
    }, "writeFileSync");
  }

  static appendFileSync(filePath_or_obj, data_arg, encoding_arg) {
    let filePath, data, encoding, durable = true;
    if (typeof filePath_or_obj === "object" && filePath_or_obj !== null && !ArrayBuffer.isView(filePath_or_obj) && !(filePath_or_obj instanceof ArrayBuffer)) {
      ({ filePath, data, encoding = "utf8" } = filePath_or_obj);
      durable = filePath_or_obj.durable !== false;
    } else {
      filePath = filePath_or_obj;
      data = data_arg;
      encoding = encoding_arg || "utf8";
    }
    const { data_buf, data_str } = toUint8Array(data);
    const eff = BaseFileManager.#effectiveDurable(filePath, durable);
    wrapSync(() => {
      const ok = op_write_or_append_file_sync(filePath, data_buf, data_str, encoding, true, eff);
      if (!ok) throw new IOError("unknown error");
      return undefined;
    }, "appendFileSync");
  }

  static #writeFileCommon(options, append) {
    const prefix = append ? "appendFile" : "writeFile";
    // durable defaults to true (crash-safe); { durable: false } opts into the
    // faster non-fsync write, but is only honored for disposable /cache//tmp
    // data (see #effectiveDurable) -- /user saves stay durable.
    const eff = BaseFileManager.#effectiveDurable(options.filePath, options.durable);
    return wrapAsync(prefix, () => {
      const { data_buf, data_str } = toUint8Array(options.data);
      const encoding = options.encoding || "utf8";
      return op_write_or_append_file(options.filePath, data_buf, data_str, encoding, append, eff)
        .then((ok) => {
          if (!ok) throw new IOError("unknown error");
        });
    }, options);
  }

  //
  // open / close
  //
  static open(options) {
    const flag = options.flag || "r";
    return wrapAsync('open', () => {
      return op_open_file(options.filePath, flag).then((fd) => ({ fd: String(fd) }));
    }, options);
  }

  static openSync({ filePath, flag = "r" }) {
    return wrapSync(() => String(op_open_file_sync(filePath, flag)), "openSync");
  }

  static close(options) {
    return wrapAsync('close', () => op_close_file(ensureFd(options.fd)), options);
  }

  static closeSync({ fd }) {
    wrapSync(() => op_close_file_sync(ensureFd(fd)), "closeSync");
  }

  //
  // copy / rename
  //
  static copyFile(options) {
    return wrapAsync('copyFile', () => op_copy_file(options.srcPath, options.destPath), options);
  }

  static copyFileSync(srcPath_or_obj, destPath_arg) {
    let srcPath, destPath;
    if (typeof srcPath_or_obj === "object" && srcPath_or_obj !== null) {
      ({ srcPath, destPath } = srcPath_or_obj);
    } else {
      srcPath = srcPath_or_obj;
      destPath = destPath_arg;
    }
    wrapSync(() => op_copy_file_sync(srcPath, destPath), "copyFileSync");
  }

  static rename(options) {
    return wrapAsync('rename', () => op_rename(options.oldPath, options.newPath), options);
  }

  static renameSync(oldPath_or_obj, newPath_arg) {
    let oldPath, newPath;
    if (typeof oldPath_or_obj === "object" && oldPath_or_obj !== null) {
      ({ oldPath, newPath } = oldPath_or_obj);
    } else {
      oldPath = oldPath_or_obj;
      newPath = newPath_arg;
    }
    wrapSync(() => op_rename_sync(oldPath, newPath), "renameSync");
  }

  //
  // fstat / ftruncate
  //
  static fstat(options) {
    return wrapAsync('fstat', () => {
      return op_fstat(ensureFd(options.fd)).then((stat) => ({ stats: new Stats(stat) }));
    }, options);
  }

  static fstatSync({ fd }) {
    return wrapSync(() => new Stats(op_fstat_sync(ensureFd(fd))), "fstatSync");
  }

  static ftruncate(options) {
    const len = Number(options.length) || 0;
    return wrapAsync('ftruncate', () => op_ftruncate(ensureFd(options.fd), len), options);
  }

  static ftruncateSync({ fd, length = 0 }) {
    const len = Number(length) || 0;
    wrapSync(() => op_ftruncate_sync(ensureFd(fd), len), "ftruncateSync");
  }

  //
  // mkdir / readdir
  //
  static mkdir(options) {
    return wrapAsync('mkdir', () => op_mkdir(options.dirPath, !!options.recursive), options);
  }

  static mkdirSync(dirPath_or_obj, recursive_arg) {
    let dirPath, recursive;
    if (typeof dirPath_or_obj === "object" && dirPath_or_obj !== null) {
      ({ dirPath, recursive = false } = dirPath_or_obj);
    } else {
      dirPath = dirPath_or_obj;
      recursive = recursive_arg ?? false;
    }
    wrapSync(() => op_mkdir_sync(dirPath, !!recursive), "mkdirSync");
  }

  static readdir(options) {
    return wrapAsync('readdir', () => {
      return op_readdir(options.dirPath).then((files) => ({ files }));
    }, options);
  }

  static readdirSync(dirPath_or_obj) {
    const dirPath = typeof dirPath_or_obj === "object" && dirPath_or_obj !== null ? dirPath_or_obj.dirPath : dirPath_or_obj;
    return wrapSync(() => op_readdir_sync(dirPath), "readdirSync");
  }

  //
  // unlink / removeSavedFile / rmdir
  //
  static removeSavedFile(options) {
    return wrapAsync('removeSavedFile', () => {
      return op_unlink(options.filePath).then(async () => {
        try {
          await untrackSavedFilePath(options.filePath);
        } catch (_) {
          // Registry cleanup is best-effort; file deletion already succeeded.
        }
      });
    }, options);
  }

  static unlink(options) {
    return wrapAsync('unlink', () => op_unlink(options.filePath), options);
  }

  static unlinkSync(filePath_or_obj) {
    const filePath = typeof filePath_or_obj === "object" && filePath_or_obj !== null ? filePath_or_obj.filePath : filePath_or_obj;
    return wrapSync(() => op_unlink_sync(filePath), "unlinkSync");
  }

  static rmdir(options) {
    return wrapAsync('rmdir', () => op_rmdir(options.dirPath, !!options.recursive), options);
  }

  static rmdirSync(dirPath_or_obj, recursive_arg) {
    let dirPath, recursive;
    if (typeof dirPath_or_obj === "object" && dirPath_or_obj !== null) {
      ({ dirPath, recursive = false } = dirPath_or_obj);
    } else {
      dirPath = dirPath_or_obj;
      recursive = recursive_arg ?? false;
    }
    wrapSync(() => op_rmdir_sync(dirPath, !!recursive), "rmdirSync");
  }

  //
  // stat
  //
  static stat(options) {
    const recursive = !!options.recursive;
    return wrapAsync('stat', () => {
      return op_stat(options.path, recursive).then((stat) => {
        if (!recursive) return { stats: new Stats(stat) };
        if (!Array.isArray(stat)) {
          return { stats: [new FileStats(options.path, stat)] };
        }
        return { stats: stat.map((item) => new FileStats(item.path, item.stat)) };
      });
    }, options);
  }

  static statSync(path_or_obj, recursive_arg) {
    let path, recursive;
    if (typeof path_or_obj === "object" && path_or_obj !== null) {
      ({ path, recursive = false } = path_or_obj);
    } else {
      path = path_or_obj;
      recursive = recursive_arg ?? false;
    }
    const rec = !!recursive;
    return wrapSync(() => {
      const raw = op_stat_sync(path, rec);
      if (!rec) return new Stats(raw);
      if (!Array.isArray(raw)) return [new FileStats(path, raw)];
      return raw.map((item) => new FileStats(item.path, item.stat));
    }, "statSync");
  }

  //
  // truncate (path) - ensure close always
  //
  static truncate(options) {
    const len = Number(options.length) || 0;
    return wrapAsync('truncate', () => {
      return op_open_file(options.filePath, "r+").then(async (fd) => {
        try {
          await op_ftruncate(fd, len);
        } finally {
          try { await op_close_file(fd); } catch (_) { }
        }
      });
    }, options);
  }

  static truncateSync({ filePath, length = 0 }) {
    const len = Number(length) || 0;
    return wrapSync(() => {
      let fd;
      try {
        fd = op_open_file_sync(filePath, "r+");
        op_ftruncate_sync(fd, len);
      } finally {
        if (fd !== undefined) {
          try { op_close_file_sync(fd); } catch (_) { }
        }
      }
      return undefined;
    }, "truncateSync");
  }

  //
  // write(fd)
  //
  static write(options) {
    return wrapAsync('write', () => {
      const { data_buf, data_str } = toUint8Array(options.data, options.offset || 0, options.length);
      const encoding = options.encoding || "utf8";
      let pos = options.position;
      if (typeof pos !== "number" || !Number.isFinite(pos)) pos = undefined;
      return op_write_file(ensureFd(options.fd), data_buf, data_str, encoding, pos)
        .then((bytesWritten) => ({ bytesWritten }));
    }, options);
  }

  static writeSync({ fd, data, offset = 0, length, encoding = "utf8", position }) {
    const { data_buf, data_str } = toUint8Array(data, offset, length);

    let pos = position;
    if (typeof pos !== "number" || !Number.isFinite(pos)) pos = undefined;

    return wrapSync(() => {
      const bytesWritten = op_write_file_sync(ensureFd(fd), data_buf, data_str, encoding, pos);
      return { bytesWritten };
    }, "writeSync");
  }

  //
  // readFile (path)
  //
  static readFile(options) {
    const encoding = options.encoding;
    const pos = typeof options.position === "number" && Number.isFinite(options.position) && options.position >= 0
      ? BigInt(Math.trunc(options.position))
      : undefined;
    const len = typeof options.length === "number" && Number.isFinite(options.length) && options.length >= 0
      ? BigInt(Math.trunc(options.length))
      : undefined;

    return wrapAsync('readFile', () => {
      return op_read_file(options.filePath, pos, len).then((data) => {
        return BaseFileManager.#decodeReadData(data, encoding);
      });
    }, options);
  }

  static readFileSync(filePath_or_obj, encoding_arg, position_arg, length_arg) {
    let filePath, encoding, position, length;
    if (typeof filePath_or_obj === "object" && filePath_or_obj !== null) {
      ({ filePath, encoding, position, length } = filePath_or_obj);
    } else {
      filePath = filePath_or_obj;
      encoding = encoding_arg;
      position = position_arg;
      length = length_arg;
    }
    const pos = typeof position === "number" && Number.isFinite(position) && position >= 0
      ? BigInt(Math.trunc(position))
      : undefined;
    const len = typeof length === "number" && Number.isFinite(length) && length >= 0
      ? BigInt(Math.trunc(length))
      : undefined;

    return wrapSync(() => {
      const raw = op_read_file_sync(filePath, pos, len);
      return BaseFileManager.#decodeReadResult(raw, encoding);
    }, "readFileSync");
  }

  static #decodeReadResult(bytes, encoding) {
    if (encoding === undefined || encoding === null || encoding === "") {
      return BaseFileManager.#exactBuffer(bytes);
    }
    return BaseFileManager.#decodeBytes(bytes, encoding);
  }

  // Resolve the effective write durability. Correctness-first: a fast
  // (non-crash-safe) write is only honored for disposable /cache and /tmp
  // data. /user (game saves) and anywhere else is always Durable regardless
  // of the requested flag -- a torn/lost save is worse than a slow one.
  // Callers pass the *requested* value (undefined/true => Durable).
  static #effectiveDurable(filePath, requestedDurable) {
    if (requestedDurable !== false) return true;
    const p = String(filePath || "");
    const disposable =
      p === "/cache" || p.startsWith("/cache/") || p === "/tmp" || p.startsWith("/tmp/");
    return !disposable;
  }

  // Return the ArrayBuffer for an op result without a defensive copy when
  // the view already covers its whole (freshly-allocated) buffer -- which is
  // the case for ToJsBuffer op returns (offset 0, full length). Only fall
  // back to slicing for a partial-window view. Accepts either a typed-array
  // view or a raw ArrayBuffer, and preserves the view's own window so a
  // future partial-window op return stays correct.
  static #exactBuffer(view) {
    if (view instanceof ArrayBuffer) {
      return view;
    }
    if (view.byteOffset === 0 && view.byteLength === view.buffer.byteLength) {
      return view.buffer;
    }
    return view.buffer.slice(view.byteOffset, view.byteOffset + view.byteLength);
  }

  static #decodeReadData(bytes, encoding) {
    return { data: BaseFileManager.#decodeReadResult(bytes, encoding) };
  }

  static #decodeBytes(bytes, encoding) {
    const enc = String(encoding).toLowerCase();
    const len = bytes.length;

    switch (enc) {
      case "utf8":
      case "utf-8":
        return op_decode_multi_formats(bytes, "utf-8");

      case "utf16le":
      case "utf-16le":
      case "ucs2":
      case "ucs-2":
        return op_decode_multi_formats(bytes, "utf-16le");

      case "latin1":
      case "binary":
        return BaseFileManager.#bytesToStringChunked(bytes, len, false);

      case "ascii":
        return BaseFileManager.#bytesToStringChunked(bytes, len, true);

      case "hex": {
        const parts = new Array(len);
        for (let i = 0; i < len; i++) {
          parts[i] = HEX_LUT[bytes[i]];
        }
        return parts.join("");
      }

      case "base64":
        return btoa(BaseFileManager.#bytesToStringChunked(bytes, len, false));

      default:
        throw new IOError(`Unsupported encoding: ${encoding}`);
    }
  }

  static #bytesToStringChunked(bytes, len, ascii) {
    const CHUNK = 8192;
    if (len <= CHUNK) {
      return ascii
        ? String.fromCharCode.apply(null, BaseFileManager.#maskAscii(bytes, 0, len))
        : String.fromCharCode.apply(null, bytes);
    }
    const parts = [];
    for (let i = 0; i < len; i += CHUNK) {
      const end = i + CHUNK < len ? i + CHUNK : len;
      const slice = bytes.subarray(i, end);
      parts.push(
        ascii
          ? String.fromCharCode.apply(null, BaseFileManager.#maskAscii(slice, 0, slice.length))
          : String.fromCharCode.apply(null, slice)
      );
    }
    return parts.join("");
  }

  static #maskAscii(bytes, start, end) {
    const out = new Uint8Array(end - start);
    for (let i = start; i < end; i++) out[i - start] = bytes[i] & 0x7F;
    return out;
  }

  //
  // unzip
  //
  static unzip(options) {
    return wrapAsync('unzip', () => op_unzip(options.zipFilePath, options.targetPath), options);
  }

  //
  // read(fd)
  //
  static read(options) {
    return wrapAsync('read', () => {
      const numFd = ensureFd(options.fd);
      const arrayBuffer = options.arrayBuffer;
      if (!arrayBuffer || !(arrayBuffer instanceof ArrayBuffer)) {
        throw new IOError("arrayBuffer must be an ArrayBuffer instance");
      }
      let offset = Math.trunc(options.offset || 0);
      // Allow offset == byteLength (a valid 0-byte read at EOF of the
      // buffer, and the only valid offset for an empty ArrayBuffer).
      if (offset < 0 || offset > ArrayBufferPrototypeGetByteLength(arrayBuffer)) {
        throw new IOError("invalid offset");
      }
      const maxLen = ArrayBufferPrototypeGetByteLength(arrayBuffer) - offset;
      const hasLength = typeof options.length === "number" && Number.isFinite(options.length);
      let readLen = hasLength ? Math.min(Math.max(0, Math.trunc(options.length)), maxLen) : maxLen;
      if (readLen < 0) throw new IOError("invalid length");

      let pos;
      if (typeof options.position === "number" && Number.isFinite(options.position) && options.position >= 0) {
        pos = BigInt(Math.trunc(options.position));
      }

      // Workers own staging bytes. Commit on the isolate before callbacks;
      // intrinsic set also rejects a destination detached while IO was pending.
      const view = new Uint8Array(arrayBuffer, offset, readLen);
      return op_read_fd_into(numFd, view, pos).then((bytes) => {
        TypedArrayPrototypeSet(view, bytes, 0);
        return { bytesRead: TypedArrayPrototypeGetByteLength(bytes), arrayBuffer };
      });
    }, options);
  }

  static readSync({ fd, arrayBuffer, offset = 0, length = 0, position }) {
    const numFd = ensureFd(fd);
    if (!arrayBuffer || !(arrayBuffer instanceof ArrayBuffer)) {
      throw new IOError("arrayBuffer must be an ArrayBuffer instance");
    }
    offset = Math.trunc(offset);
    // Allow offset == byteLength (valid 0-byte read; also the only valid
    // offset for an empty ArrayBuffer).
    if (offset < 0 || offset > ArrayBufferPrototypeGetByteLength(arrayBuffer)) {
      throw new IOError("invalid offset");
    }
    const maxLen = ArrayBufferPrototypeGetByteLength(arrayBuffer) - offset;
    const hasLength = typeof length === "number" && Number.isFinite(length);
    let readLen = hasLength ? Math.min(Math.max(0, Math.trunc(length)), maxLen) : maxLen;
    if (readLen < 0) throw new IOError("invalid length");
    let pos;
    if (typeof position === "number" && Number.isFinite(position) && position >= 0) {
      pos = BigInt(Math.trunc(position));
    }

    return wrapSync(() => {
      // Zero-copy: fill the caller's ArrayBuffer window directly.
      const view = new Uint8Array(arrayBuffer, offset, readLen);
      const bytesRead = op_read_fd_into_sync(numFd, view, pos);
      return { bytesRead: Number(bytesRead), arrayBuffer };
    }, "readSync");
  }

  //
  // readCompressedFile
  //
  static readCompressedFile(options) {
    return wrapAsync('readCompressedFile', () => {
      if (options.compressionAlgorithm !== "br") {
        throw new IOError("unsupported compressionAlgorithm");
      }
      return op_read_compressed_file(options.filePath).then((data) => {
        // Preserve the op result's own window (#exactBuffer handles both a
        // typed-array view and a raw ArrayBuffer).
        return { data: BaseFileManager.#exactBuffer(data) };
      });
    }, options);
  }

  static readCompressedFileSync(obj) {
    const filePath = obj.filePath;
    const compressionAlgorithm = obj.compressionAlgorithm;
    if (compressionAlgorithm !== "br") {
      throw new IOError("unsupported compressionAlgorithm");
    }
    return wrapSync(() => {
      const data = op_read_compressed_file_sync(filePath);
      return BaseFileManager.#exactBuffer(data);
    }, "readCompressedFileSync");
  }

  //
  // readZipEntry
  //
  static readZipEntry(options) {
    const encoding = options.encoding;
    const entries = options.entries;
    const entriesJson = JSON.stringify({ encoding, entries });
    return wrapAsync('readZipEntry', () => {
      // Rust returns a list of `{path, text?, bytes?, errMsg}`; `bytes` is a
      // Uint8Array over a buffer sized exactly to the entry, so handing back
      // `.buffer` is the whole binary path. It used to be a base64 string that
      // JS decoded a byte at a time.
      return op_read_zip_entry(options.filePath, entriesJson)
        .then(zipEntriesFromOpResult);
    }, options);
  }

  //
  // saveFile
  //
  static saveFile(options) {
    return wrapAsync('saveFile', () => {
      const tempFilePath = options.tempFilePath;
      if (typeof tempFilePath !== "string" || tempFilePath.length === 0) {
        throw new IOError("tempFilePath is required");
      }
      const savedFilePath = resolveSavedFilePath(options.filePath, tempFilePath);
      return op_rename(tempFilePath, savedFilePath)
        .catch(async () => {
          await op_copy_file(tempFilePath, savedFilePath);
          await op_unlink(tempFilePath);
        })
        .then(() => {
          return trackSavedFilePath(savedFilePath).catch(() => undefined);
        }).then(() => {
          return { savedFilePath };
        });
    }, options);
  }

  static saveFileSync(tempFilePath_or_obj, filePath_arg) {
    let tempFilePath, filePath;
    if (typeof tempFilePath_or_obj === "object" && tempFilePath_or_obj !== null) {
      ({ tempFilePath, filePath } = tempFilePath_or_obj);
    } else {
      tempFilePath = tempFilePath_or_obj;
      filePath = filePath_arg;
    }
    if (typeof tempFilePath !== "string" || tempFilePath.length === 0) {
      throw new IOError("tempFilePath is required");
    }

    return wrapSync(() => {
      const savedFilePath = resolveSavedFilePath(filePath, tempFilePath);
      try {
        op_rename_sync(tempFilePath, savedFilePath);
      } catch (_) {
        op_copy_file_sync(tempFilePath, savedFilePath);
        op_unlink_sync(tempFilePath);
      }
      try {
        trackSavedFilePathSync(savedFilePath);
      } catch (_) {
        // Registry update is best-effort; save already succeeded.
      }
      return savedFilePath;
    }, "saveFileSync");
  }

  //
  // getFileInfo
  //
  static getFileInfo(options) {
    return wrapAsync('getFileInfo', () => {
      return op_get_file_info(options.filePath, options.digestAlgorithm || "md5")
        .then(([size, digest]) => ({ size, digest }));
    }, options);
  }

  //
  // getSavedFileList
  //
  static getSavedFileList(options) {
    return wrapAsync('getSavedFileList', () => {
      return op_list_saved_files(SAVE_FILE_DIR, "saved_")
        .then(async (fileList) => {
          const seen = new Set(fileList.map((item) => item.filePath));
          const extras = [];
          const persisted = await loadSavedFileRegistry();
          for (const filePath of persisted) {
            rememberSavedFilePath(filePath);
          }
          for (const filePath of _trackedSavedFilePaths) {
            if (seen.has(filePath)) continue;
            try {
              const [size] = await op_get_file_info(filePath, "md5");
              const stat = await op_stat(filePath, false);
              extras.push({ filePath, size, createTime: stat.mtime || 0 });
            } catch (_) {
              // Ignore deleted or inaccessible saved paths.
            }
          }
          return { fileList: fileList.concat(extras) };
        });
    }, options || {});
  }
}

const fileSystemManager = BaseFileManager;

function getFileSystemManager() {
  return fileSystemManager;
}

export { BaseFileManager, getFileSystemManager, zipEntriesFromOpResult };
