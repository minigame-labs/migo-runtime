// Ops that return a promise the host settles.
//
// Most of them are service requests: the op's arguments are written in the
// shape its Rust signature takes, the service stream carries them (see
// `service.mjs`), and the host runs the same service code the embedded op
// calls -- `migo_services` -- so the answer and the error class are the ones
// every other platform gives.

import { engineHost } from "./engine-host.mjs";
import { drained } from "./engine-frames.mjs";
import { smiU32, stringOf, toI32 } from "./op-args.mjs";
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
