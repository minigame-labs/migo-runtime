// `core.read`, `core.readAll`, `core.close` and `core.tryClose`.
//
// These are members of deno's `core` object rather than ops, and they act on
// the handles a `fetch` hands out. Those handles are the network service's
// (`migo_services::network::resources`), so on this lane the host answers them
// exactly as it answers the ops beside them: `read` is a request, the two
// closes are commands, and their numbers are in `contracts/runtime/
// service-ops.json` under `core_read`, `core_close` and `core_try_close`.
// The lanes are decided in `contracts/runtime/op-boundary.json`'s
// `core_members` table.
//
// `readAll` crosses nothing of its own: it is the same loop deno runs in Rust,
// built here out of `read`, so a body read whole and a body read in chunks take
// the same path and the host holds one chunk at a time either way.

import { READ_CHUNK_BYTES } from "./files.mjs";
import { engineHost } from "./engine-host.mjs";
import { servicesOf } from "./lane-async.mjs";
import { SERVICE_OP } from "./service-ops.mjs";

/** How much `readAll` asks for at a time before it knows the size. */
const READ_ALL_CHUNK_BYTES = 64 * 1024;

/**
 * Read into `view`, and answer how many bytes arrived -- zero at the end of the
 * body, which is how every caller stops.
 *
 * The bytes are asked for by count and written into the caller's view when the
 * answer lands, which is the embedded op's shape: deno's `op_read` fills a
 * buffer it took from the caller, and neither writes it while the read is in
 * flight. One answer is bounded by the caller's view and by the largest read
 * the lanes ask for, so no response body -- however large -- is asked for in
 * one reply.
 */
export function read(rid, view) {
  const wanted = Math.min(view.byteLength, READ_CHUNK_BYTES);
  return servicesOf(engineHost())
    .request(SERVICE_OP.core_read, (w) => {
      w.u32(rid);
      w.u32(wanted);
    })
    .then((bytes) => {
      view.set(bytes, 0);
      return bytes.byteLength;
    });
}

/**
 * The whole body, as one Uint8Array.
 *
 * Chunks are held and joined once, rather than grown by doubling: a doubling
 * buffer copies what it already holds every time it grows, which for a body
 * read in 64 KiB pieces is the body copied several times over.
 */
export function readAll(rid) {
  const chunks = [];
  let total = 0;
  const buffer = new Uint8Array(READ_ALL_CHUNK_BYTES);
  const next = () =>
    read(rid, buffer).then((count) => {
      if (count === 0) {
        const all = new Uint8Array(total);
        let filled = 0;
        for (const chunk of chunks) {
          all.set(chunk, filled);
          filled += chunk.byteLength;
        }
        return all;
      }
      chunks.push(buffer.slice(0, count));
      total += count;
      return next();
    });
  return next();
}

/** Close `rid`. A handle that names nothing is the host's error to report. */
export function close(rid) {
  servicesOf(engineHost()).command(SERVICE_OP.core_close, (w) => w.u32(rid));
}

/** Close `rid` if it is open, and say nothing if it is not. */
export function tryClose(rid) {
  servicesOf(engineHost()).command(SERVICE_OP.core_try_close, (w) => w.u32(rid));
}
