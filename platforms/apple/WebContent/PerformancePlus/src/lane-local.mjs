// Ops the producer answers itself, without crossing to the host.
//
// Each is the Rust op of the same name, re-expressed against state the producer
// owns: its clocks, its resource ids, the attributes content asked for, and the
// host's surface and lifecycle as the engine host was told them. The lane and the
// reason it is correct are in contracts/runtime/op-boundary.json; that this file
// implements only ops the contract gives this lane is checked by
// scripts/gen-performance-plus-engine.py.

import { platform } from "./engine-core.mjs";
import { engineHost } from "./engine-host.mjs";

// ---- time -------------------------------------------------------------------

/// Elapsed time since the session was bound, as the Rust op writes it: u32
/// seconds then u32 nanoseconds, little-endian, into the caller's 8 bytes.
export function op_now(buffer) {
  if (buffer.byteLength < 8) return;
  const elapsedMs = platform.now() - engineHost().startedAt;
  const seconds = Math.floor(elapsedMs / 1000);
  const nanos = Math.min(999_999_999, Math.round((elapsedMs - seconds * 1000) * 1e6));
  const view = new DataView(buffer.buffer, buffer.byteOffset, 8);
  view.setUint32(0, seconds >>> 0, true);
  view.setUint32(4, nanos >>> 0, true);
}

/// Unix time in microseconds, as a Number (the Rust op is `#[number] u64`).
export function op_now_us() {
  return Math.floor((platform.timeOrigin + platform.now()) * 1000);
}

/// Whether the host has put timers in the background.
export function op_timer_is_backgrounded() {
  return engineHost().state.backgrounded;
}

// ---- canvases ---------------------------------------------------------------

/// The main canvas (id 1) is the surface; its size is the one the host
/// described. Offscreen canvases are created on the stream lane, which has not
/// landed, so no other id can exist yet -- and the Rust op refuses an unknown id
/// the same way.
export function op_get_canvas_info(id) {
  const { state } = engineHost();
  if (id === 1) return [state.surfaceWidth, state.surfaceHeight];
  throw new Error(`canvas ${id} not found`);
}

// ---- WebGL ------------------------------------------------------------------

let nextResourceId = 1;

/// The Rust allocator exactly: from 1, wrapping past u32::MAX back to 1, never 0.
export function op_alloc_gl_resource_id() {
  const id = nextResourceId;
  nextResourceId = (nextResourceId + 1) >>> 0;
  if (nextResourceId === 0) nextResourceId = 1;
  return id;
}

const POWER_PREFERENCE = ["default", "high-performance", "low-power"];
const contextAttributes = new Map();

/// What content asked for when it created a context, recorded per canvas.
export function op_webgl_record_attributes(
  canvasId,
  alpha,
  antialias,
  depth,
  stencil,
  premultipliedAlpha,
  preserveDrawingBuffer,
  powerPreference,
  failIfMajorPerformanceCaveat,
  desynchronized,
  xrCompatible,
) {
  contextAttributes.set(canvasId >>> 0, {
    alpha: !!alpha,
    antialias: !!antialias,
    depth: !!depth,
    stencil: !!stencil,
    premultipliedAlpha: !!premultipliedAlpha,
    preserveDrawingBuffer: !!preserveDrawingBuffer,
    powerPreference: POWER_PREFERENCE[powerPreference] ?? "default",
    failIfMajorPerformanceCaveat: !!failIfMajorPerformanceCaveat,
    desynchronized: !!desynchronized,
    xrCompatible: !!xrCompatible,
  });
}

/// The recorded attributes, or WebGL 1.0's defaults (section 5.2.1) with the
/// stencil the backend always has -- `ContextAttributes::default()` in Rust.
export function op_webgl_get_context_attributes(canvasId) {
  const recorded = contextAttributes.get(canvasId >>> 0);
  if (recorded) return { ...recorded };
  return {
    alpha: true,
    antialias: true,
    depth: true,
    stencil: true,
    premultipliedAlpha: true,
    preserveDrawingBuffer: false,
    powerPreference: "default",
    failIfMajorPerformanceCaveat: false,
    desynchronized: false,
    xrCompatible: false,
  };
}

// The facade's own validation errors, queued per canvas exactly as the Rust
// `WebGLErrorState` queues them: 256 per context, the last slot reserved for an
// OUT_OF_MEMORY sentinel that reports the truncation. `getError` drains these
// before asking the host for its own, which lands with the synchronous lane.
const MAX_ERRORS_PER_CONTEXT = 256;
const INVALID_ENUM = 0x0500;
const INVALID_VALUE = 0x0501;
const INVALID_OPERATION = 0x0502;
const OUT_OF_MEMORY = 0x0505;
const errorQueues = new Map();

function pushError(canvasId, code) {
  let queue = errorQueues.get(canvasId);
  if (!queue) errorQueues.set(canvasId, (queue = []));
  if (queue.length < MAX_ERRORS_PER_CONTEXT - 1) {
    queue.push(code);
  } else if (queue[queue.length - 1] !== OUT_OF_MEMORY) {
    queue.push(OUT_OF_MEMORY);
  }
}

/// Queue a WebGL error the producer found itself, for `getError`. Not an op: the
/// stream lane calls it for a call it could not encode.
export function recordProducerError(canvasId, code) {
  pushError(canvasId >>> 0, code);
}

/// Oldest queued producer-side error for a canvas, or 0. Not an op: the sync
/// lane's `op_webgl_get_error` calls it before crossing.
export function drainProducerError(canvasId) {
  const queue = errorQueues.get(canvasId >>> 0);
  return queue && queue.length > 0 ? queue.shift() : 0;
}

/// Only the four codes content may record; anything else is INVALID_OPERATION.
export function op_webgl_record_error(canvasId, code) {
  const valid = code === INVALID_ENUM || code === INVALID_VALUE || code === INVALID_OPERATION || code === OUT_OF_MEMORY;
  pushError(canvasId >>> 0, valid ? code : INVALID_OPERATION);
}

export function op_webgl_record_out_of_memory(canvasId) {
  pushError(canvasId >>> 0, OUT_OF_MEMORY);
}

/// Whether the host reported the GL context lost and not yet restored.
export function op_gl_is_context_lost() {
  return engineHost().state.contextLost;
}

// ---- the text-texture cache ---------------------------------------------------

/**
 * `op_text_cache_peek_pin`: a miss, always, and that is the truth rather than a
 * placeholder.
 *
 * The cache it asks about is a host-side optimisation for one pattern -- a 2D
 * canvas whose whole contents are a single label, re-rendered and re-uploaded
 * every frame, which is what Cocos does for score text. A host that keeps such
 * a cache can answer "hit" and skip the paint; this host keeps none, so nothing
 * is cached and the text is painted. The engine's facade takes 0 as "render
 * normally", which is exactly right.
 *
 * Answered here rather than on the synchronous lane because a round trip would
 * block the Worker once per `fillText` to be told something that cannot change
 * until the cache exists. When it does, this moves back to the sync lane (see
 * contracts/runtime/op-boundary.json).
 */
export function op_text_cache_peek_pin() {
  return 0;
}

// ---- what the host knows about the device -----------------------------------
//
// A game reads these before its first frame -- `migo.getWindowInfo()` to lay
// itself out, `getSystemInfoSync()` for the model and the safe area -- and each
// is synchronous. None of it changes during a session, so the host hands the
// JSON over at startup (see `engine-host.mjs`) and these answer from it without
// crossing. That is what the contract means by the `local` lane for them.
//
// When the host described nothing, they fail the way they do on a platform with
// no device services: the op's own message. A plausible screen size nobody
// measured would be worse than a failure -- a game would lay itself out to it.

function described(key, call) {
  const profile = engineHost().device;
  const json = profile === null ? undefined : profile[key];
  if (json === undefined) {
    // The message the Rust op raises, so content sees one failure, not two.
    throw new Error(`${call}:fail not supported`);
  }
  return json;
}

export function op_get_window_info() {
  return described("windowInfo", "getWindowInfo");
}

export function op_get_device_info() {
  return described("deviceInfo", "getDeviceInfo");
}

export function op_get_system_settings() {
  return described("systemSettings", "getSystemSetting");
}

export function op_get_menu_button_rect() {
  return described("menuButtonRect", "getMenuButtonBoundingClientRect");
}

export function op_get_network_type() {
  return described("networkType", "getNetworkType");
}
