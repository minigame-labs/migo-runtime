// The file system's service calls, run by the producer and answered by the host.
//
//   emit-file-calls.mjs write <dir>  -- run the script below through the lane
//                                       functions, recording every service call
//                                       they make, for the Rust replay
//   emit-file-calls.mjs read  <dir>  -- run the same script again, answered by
//                                       what the host answered when it replayed
//                                       those calls, and check the results
//
// Between the two, engine/crates/core/src/runtime/external_services.rs
// (`the_producer_s_file_calls_run_on_the_host`) replays the recorded calls, in
// order, through the host's own dispatch on a real mounted game sandbox and
// writes each answer. So the arguments are the ones the host takes, the host
// runs the same migo_services::fs code the embedded ops call, and the results
// are what the producer makes of the host's real answers: every one of the 41
// file ops and `require`, sync and awaited.
//
// Driven by scripts/test-performance-plus-engine-contract.sh.

import assert from "node:assert/strict";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { bindEngineHost, readEngineSessionConfig } from "../src/engine-host.mjs";
import { FrameSession } from "../src/frame-session.mjs";
import * as asyncLane from "../src/lane-async.mjs";
import * as syncLane from "../src/lane-sync.mjs";
import { decodeServiceOutcome, OUTCOME_OK } from "../src/service.mjs";
import { SERVICE_OP } from "../src/service-ops.mjs";
import { ValueWriter } from "../src/service-value.mjs";

const [mode, dir] = process.argv.slice(2);
if ((mode !== "write" && mode !== "read") || !dir) {
  console.error("usage: emit-file-calls.mjs write|read <dir>");
  process.exit(2);
}

const NAME_OF = new Map(Object.entries(SERVICE_OP).map(([name, id]) => [id, name]));

// A zip of two stored entries: hi.txt ("hi") and data.bin (00 ff 80).
const ZIP = Uint8Array.from(
  atob(
    "UEsDBBQAAAAAAAAAIVysKpPYAgAAAAIAAAAGAAAAaGkudHh0aGlQSwMEFAAAAAAAAAAhXECn3YEDAAAAAwAAAAgAAABkYXRhLmJpbgD/gFBLAQIUAxQAAAAAAAAAIVysKpPYAgAAAAIAAAAGAAAAAAAAAAAAAACAAQAAAABoaS50eHRQSwECFAMUAAAAAAAAACFcQKfdgQMAAAADAAAACAAAAAAAAAAAAAAAgAEmAAAAZGF0YS5iaW5QSwUGAAAAAAIAAgBqAAAATwAAAAAA",
  ),
  (char) => char.charCodeAt(0),
);

/** The descriptor the recording run answers every open with. */
const RECORDED_FD = 7;

// ---- the two transports ---------------------------------------------------------

const calls = []; // write mode: what the lanes sent
let answers = []; // read mode: what the host answered, in order
let next = 0;

/** The host's answer to the next call, as the reply bytes a transport carries. */
function answerFor(op) {
  if (mode === "write") {
    // Only the shape the lane's result builder needs to proceed; the real
    // values come back in read mode.
    const w = new ValueWriter(64);
    w.word(OUTCOME_OK);
    canned(op, w);
    return w.finish();
  }
  const answer = answers[next];
  next += 1;
  assert.equal(answer.op, NAME_OF.get(op), `answer ${next} is for ${answer.op}, the call was ${NAME_OF.get(op)}`);
  return Uint8Array.from(Buffer.from(answer.outcome, "hex"));
}

function canned(op, w) {
  const name = NAME_OF.get(op);
  const stat = () => {
    w.array(6);
    w.u32(0);
    w.f64(0);
    w.f64(0);
    w.f64(0);
    w.bool(true);
    w.bool(false);
  };
  if (/^op_open_file/.test(name)) w.u32(RECORDED_FD);
  else if (/^op_(access|write_or_append_file)/.test(name)) w.bool(true);
  else if (/^op_fstat/.test(name)) stat();
  else if (/^op_stat/.test(name)) {
    w.array(2);
    w.u32(0);
    stat();
  } else if (/^op_write_file/.test(name)) w.u64(0n);
  else if (/^op_read_(file|fd|compressed_file)/.test(name)) w.bytes(new Uint8Array(0));
  else if (/^op_(readdir|read_zip_entry|list_saved_files)/.test(name)) w.array(0);
  else if (/^op_get_file_info/.test(name)) {
    w.array(2);
    w.f64(0);
    w.str("");
  } else if (name === "op_require_resolve_and_read") {
    w.array(3);
    w.str("");
    w.str("");
    w.str("");
  } else w.null();
}

function record(shape, op, writeArgs) {
  const w = new ValueWriter(64);
  writeArgs?.(w);
  calls.push({ shape, op: NAME_OF.get(op), args: Buffer.from(w.finish()).toString("hex") });
}

const services = {
  request(op, writeArgs) {
    if (mode === "write") record("async", op, writeArgs);
    const outcome = decodeServiceOutcome(answerFor(op));
    if (outcome.ok) return Promise.resolve(outcome.value);
    return Promise.reject(Object.assign(new Error(outcome.message), { name: outcome.className }));
  },
  command() {
    throw new Error("no file op is a command");
  },
  flush() {
    return 0n;
  },
};

const sync = {
  call(call) {
    // The call body is the op then its arguments; the arguments are recorded.
    const view = new DataView(call.params.buffer, call.params.byteOffset, call.params.byteLength);
    const op = view.getUint32(0, true);
    if (mode === "write") {
      calls.push({
        shape: "sync",
        op: NAME_OF.get(op),
        args: Buffer.from(call.params.subarray(4)).toString("hex"),
      });
    }
    return answerFor(op);
  },
};

bindEngineHost({
  session: new FrameSession({ send() {}, sendControl() {} }),
  identity: readEngineSessionConfig({
    launchNonce: "0x0123456789abcdeffedcba9876543210",
    runtimeGeneration: "1",
    surfaceGeneration: "1",
    resourceEpoch: "0",
    surfaceWidth: 64,
    surfaceHeight: 64,
  }),
  socketCeilingBytes: 64 * 1024,
  sync,
  services,
  report() {},
});

// ---- the script -------------------------------------------------------------------
//
// Each step returns what it checks in read mode. The same calls, in the same
// order, in both modes: nothing branches on an answer.

const text = (bytes) => new TextDecoder().decode(bytes);
const checks = [];
const check = (what, fn) => checks.push([what, fn]);

/** A call expected to fail on the host: its error, or a note that it did not. */
function failure(fn) {
  try {
    fn();
  } catch (error) {
    return error;
  }
  return null;
}
async function rejection(promise) {
  try {
    await promise;
  } catch (error) {
    return error;
  }
  return null;
}

async function script() {
  const s = syncLane;
  const a = asyncLane;

  // Synchronous.
  s.op_mkdir_sync("/user/saves", true);
  const wrote = s.op_write_or_append_file_sync("/user/saves/a.txt", null, "hello", "utf8", false, true);
  check("writeFileSync answers true", () => assert.equal(wrote, true));
  s.op_write_or_append_file_sync("/user/saves/a.txt", Uint8Array.of(0x21), null, null, true, true);
  const exists = s.op_access_sync("/user/saves/a.txt");
  check("accessSync finds it", () => assert.equal(exists, true));
  const whole = s.op_read_file_sync("/user/saves/a.txt", null, null);
  check("readFileSync reads the write and the append", () => {
    assert.ok(whole instanceof Uint8Array);
    assert.equal(text(whole), "hello!");
  });
  const range = s.op_read_file_sync("/user/saves/a.txt", 1n, 3n);
  check("a ranged readFileSync", () => assert.equal(text(range), "ell"));
  const stat = s.op_stat_sync("/user/saves/a.txt", false);
  check("statSync is the FileStat object, fields in order", () => {
    assert.deepEqual(Object.keys(stat), ["mode", "size", "atime", "mtime", "is_file", "is_directory"]);
    assert.equal(stat.size, 6);
    assert.equal(stat.is_file, true);
    assert.equal(stat.is_directory, false);
  });
  const tree = s.op_stat_sync("/user/saves", true);
  check("a recursive statSync is [{path, stat}]", () => {
    assert.ok(Array.isArray(tree));
    assert.deepEqual(
      tree.map((entry) => entry.path),
      ["a.txt"],
    );
    assert.equal(tree[0].stat.size, 6);
  });
  const names = s.op_readdir_sync("/user/saves");
  check("readdirSync lists it", () => assert.deepEqual(names, ["a.txt"]));
  s.op_copy_file_sync("/user/saves/a.txt", "/user/saves/b.txt");
  s.op_rename_sync("/user/saves/b.txt", "/user/saves/c.txt");
  const info = s.op_get_file_info_sync("/user/saves/a.txt", "md5");
  check("getFileInfoSync is [size, digest]", () => {
    assert.equal(info[0], 6);
    assert.match(info[1], /^[0-9a-f]{32}$/);
  });
  const fd = s.op_open_file_sync("/user/saves/a.txt", "r+");
  const fdStat = s.op_fstat_sync(fd);
  check("fstatSync", () => assert.equal(fdStat.size, 6));
  const written = s.op_write_file_sync(fd, null, "J", "utf8", 0);
  check("writeSync answers a BigInt count", () => assert.equal(written, 1n));
  const head = s.op_read_fd_sync(fd, 3n, 0n);
  check("readSync by descriptor", () => assert.equal(text(head), "Jel"));
  const window = new Uint8Array(10);
  const filled = s.op_read_fd_into_sync(fd, window, 1n);
  check("readSync into a buffer fills it and counts, as a Number", () => {
    assert.equal(filled, 5);
    assert.equal(text(window.subarray(0, 5)), "ello!");
  });
  s.op_ftruncate_sync(fd, 2);
  s.op_close_file_sync(fd);
  const truncated = s.op_read_file_sync("/user/saves/a.txt", null, null);
  check("ftruncateSync kept two bytes", () => assert.equal(text(truncated), "Je"));
  const notBrotli = failure(() => s.op_read_compressed_file_sync("/user/saves/a.txt"));
  check("readCompressedFileSync of plain text is an IOError", () => assert.equal(notBrotli?.name, "IOError"));
  s.op_unlink_sync("/user/saves/c.txt");
  const readOnly = failure(() =>
    s.op_write_or_append_file_sync("/code/x.js", Uint8Array.of(1), null, null, false, true),
  );
  check("the package is read-only, in the op's words", () => {
    assert.equal(readOnly?.name, "IOError");
    assert.equal(readOnly.message, "Permission denied: /code/x.js");
  });
  const module = s.op_require_resolve_and_read("./js/main", "");
  check("require reads the package module", () => {
    assert.deepEqual(Object.keys(module), ["code", "abs_path", "dir"]);
    assert.equal(module.code, "module.exports = 7;");
    assert.ok(module.abs_path.endsWith("/js/main.js"), module.abs_path);
  });

  // Awaited.
  await a.op_mkdir("/user/async", false);
  const stored = await a.op_write_or_append_file("/user/async/f.bin", Uint8Array.of(1, 2, 3, 4), null, null, false, true);
  check("writeFile answers true", () => assert.equal(stored, true));
  const accessAnswer = await a.op_access("/user/async/f.bin");
  check("access", () => assert.equal(accessAnswer, true));
  const bytes = await a.op_read_file("/user/async/f.bin", null, null);
  check("readFile", () => assert.deepEqual([...bytes], [1, 2, 3, 4]));
  const asyncStat = await a.op_stat("/user/async/f.bin", false);
  check("stat", () => assert.equal(asyncStat.size, 4));
  const asyncNames = await a.op_readdir("/user/async");
  check("readdir", () => assert.deepEqual(asyncNames, ["f.bin"]));
  await a.op_copy_file("/user/async/f.bin", "/user/async/g.bin");
  await a.op_rename("/user/async/g.bin", "/user/async/h.bin");
  const digest = await a.op_get_file_info("/user/async/f.bin", "sha256");
  check("getFileInfo", () => {
    assert.equal(digest[0], 4);
    assert.match(digest[1], /^[0-9a-f]{64}$/);
  });
  const afd = await a.op_open_file("/user/async/f.bin", "r+");
  const afdStat = await a.op_fstat(afd);
  check("fstat", () => assert.equal(afdStat.size, 4));
  const count = await a.op_write_file(afd, Uint8Array.of(9), null, null, 0n);
  check("write answers a BigInt", () => assert.equal(count, 1n));
  const two = await a.op_read_fd(afd, 2n, 0n);
  check("read by descriptor", () => assert.deepEqual([...two], [9, 2]));
  const staged = await a.op_read_fd_into(afd, new Uint8Array(8), 0n);
  check("read into answers the filled bytes, for the facade to copy", () =>
    assert.deepEqual([...staged], [9, 2, 3, 4]),
  );
  await a.op_ftruncate(afd, 2);
  await a.op_close_file(afd);
  const asyncNotBrotli = await rejection(a.op_read_compressed_file("/user/async/f.bin"));
  check("readCompressedFile of plain bytes rejects with IOError", () =>
    assert.equal(asyncNotBrotli?.name, "IOError"),
  );
  await a.op_write_or_append_file("/user/async/pack.zip", ZIP, null, null, false, true);
  const entries = await a.op_read_zip_entry(
    "/user/async/pack.zip",
    JSON.stringify({ entries: [{ path: "hi.txt", encoding: "utf8" }, { path: "data.bin" }] }),
  );
  check("readZipEntry: text, bytes, absent fields left out", () => {
    assert.deepEqual(entries[0], { path: "hi.txt", text: "hi", errMsg: "" });
    assert.deepEqual(Object.keys(entries[1]), ["path", "bytes", "errMsg"]);
    assert.deepEqual([...entries[1].bytes], [0, 255, 128]);
  });
  await a.op_unzip("/user/async/pack.zip", "/user/unzipped");
  const unzipped = s.op_readdir_sync("/user/unzipped").sort();
  check("unzip extracted both entries", () => assert.deepEqual(unzipped, ["data.bin", "hi.txt"]));
  const saved = await a.op_list_saved_files("/user/async", "f");
  check("listSavedFiles is [{filePath, size, createTime}]", () => {
    assert.deepEqual(Object.keys(saved[0]), ["filePath", "size", "createTime"]);
    assert.equal(saved[0].filePath, "/user/async/f.bin");
    assert.equal(saved[0].size, 2);
  });
  await a.op_unlink("/user/async/h.bin");
  await a.op_rmdir("/user/async", true);
  // A path that is not there is the op's IOError, not `false`: the facade
  // turns it into the API's failure.
  const gone = failure(() => s.op_access_sync("/user/async"));
  check("rmdir removed the tree", () => {
    assert.equal(gone?.name, "IOError");
    assert.match(gone.message, /^\[NotFound\]/);
  });
  s.op_rmdir_sync("/user/saves", true);
}

// ---- run ----------------------------------------------------------------------------

if (mode === "read") {
  answers = JSON.parse(readFileSync(join(dir, "answers.json"), "utf8"));
}
await script();

if (mode === "write") {
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, "calls.json"), JSON.stringify(calls, null, 1));
  const ops = new Set(calls.map((call) => call.op));
  // The file ops and `require`: numbered together, and the contract is append-only.
  const fileOps = Object.keys(SERVICE_OP).filter(
    (name) => SERVICE_OP[name] >= SERVICE_OP.op_access && SERVICE_OP[name] <= SERVICE_OP.op_require_resolve_and_read,
  );
  const missing = fileOps.filter((name) => !ops.has(name));
  assert.deepEqual(missing, [], "the script leaves ops uncalled");
  console.log(`wrote ${calls.length} file calls covering ${ops.size} ops to ${dir}`);
} else {
  assert.equal(next, answers.length, `the script took ${next} of ${answers.length} answers`);
  let failed = 0;
  for (const [what, fn] of checks) {
    try {
      await fn();
      console.log(`  ok   ${what}`);
    } catch (error) {
      failed += 1;
      console.log(`  FAIL ${what}\n       ${error.message}`);
    }
  }
  if (failed > 0) {
    console.log(`FAIL: ${failed} of ${checks.length} checks`);
    process.exit(1);
  }
  console.log(`PASS: ${checks.length} checks against ${answers.length} host answers`);
}
