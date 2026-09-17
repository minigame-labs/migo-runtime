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
