// Ops the producer answers itself, without crossing to the host.
//
// Each is the Rust op of the same name, re-expressed against state the producer
// owns: its clocks, its resource ids, the attributes content asked for, and the
// host's surface and lifecycle as the engine host was told them. The lane and the
// reason it is correct are in contracts/runtime/op-boundary.json; that this file
// implements only ops the contract gives this lane is checked by
// scripts/gen-performance-plus-engine.py.

import { bytesOf, f32BitsOf, smiU32, smiU8, stringOf, toBool } from "./op-args.mjs";
import { platform } from "./engine-core.mjs";
import { decodeBytes, encodeString } from "./text-codec.mjs";
import { engineHost } from "./engine-host.mjs";

// ---- time -------------------------------------------------------------------

/// Elapsed time since the session was bound, as the Rust op writes it: u32
/// seconds then u32 nanoseconds, little-endian, into the caller's 8 bytes.
export function op_now(buf) {
  const buffer = bytesOf(buf, "buf");
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

/// The sizes of the canvases this producer created, by id.
///
/// The host has the real ones -- it allocated the surfaces -- but asking it
/// would be a synchronous round trip on the one call every canvas makes in its
/// constructor, and the answer is one this side already knows: the producer
/// chose the size in `op_create_offscreen_canvas` and changes it in
/// `op_resize_canvas`, which is exactly what `canvas.width` means. Contract
/// `local_answer`, and the reason the op is on the local lane.
const canvasSizes = new Map();

/// The next canvas id this producer hands out.
///
/// From `shared::protocol::render_cmd::PRODUCER_CANVAS_ID_BASE`, which is where
/// the in-process runtime's own counter starts, and below which the renderer
/// refuses a registration: the renderer allocates its ids from 2 upwards, so a
/// caller that allocated from 2 as well would name a canvas the renderer is
/// about to create.
const PRODUCER_CANVAS_ID_BASE = 1 << 24;
let nextCanvasId = PRODUCER_CANVAS_ID_BASE;

/// Take an id for a canvas the stream lane is about to register, and record the
/// size it is being created at.
export function allocateCanvas(width, height) {
  const id = nextCanvasId >>> 0;
  nextCanvasId += 1;
  canvasSizes.set(id, [width, height]);
  return id;
}

/// A resize this producer sent, as `canvas.width`/`height` will read it back.
export function recordCanvasSize(id, width, height) {
  const size = canvasSizes.get(id);
  if (size === undefined) return;
  if (width !== null) size[0] = width;
  if (height !== null) size[1] = height;
}

/// A canvas the content threw away; a later `op_get_canvas_info` for it is the
/// "not found" the in-process op answers once the render thread has dropped it.
export function forgetCanvas(id) {
  canvasSizes.delete(id);
}

/// The main canvas (id 1) is the surface; its size is the one the host
/// described. Every other id is one this producer allocated, and the size is
/// the one it chose -- an id from neither is refused, as the Rust op refuses an
/// id the render thread does not hold.
export function op_get_canvas_info(id) {
  const canvasId = smiU32(id, "id");
  const { state } = engineHost();
  if (canvasId === 1) return [state.surfaceWidth, state.surfaceHeight];
  const size = canvasSizes.get(canvasId);
  if (size !== undefined) return [size[0], size[1]];
  throw new Error(`canvas ${canvasId} not found`);
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
  contextAttributes.set(smiU32(canvasId, "canvas_id"), {
    alpha: toBool(alpha, "alpha"),
    antialias: toBool(antialias, "antialias"),
    depth: toBool(depth, "depth"),
    stencil: toBool(stencil, "stencil"),
    premultipliedAlpha: toBool(premultipliedAlpha, "premultiplied_alpha"),
    preserveDrawingBuffer: toBool(preserveDrawingBuffer, "preserve_drawing_buffer"),
    powerPreference: POWER_PREFERENCE[smiU8(powerPreference, "power_preference")] ?? "default",
    failIfMajorPerformanceCaveat: toBool(failIfMajorPerformanceCaveat, "fail_if_major_performance_caveat"),
    desynchronized: toBool(desynchronized, "desynchronized"),
    xrCompatible: toBool(xrCompatible, "xr_compatible"),
  });
}

/// The recorded attributes, or WebGL 1.0's defaults (section 5.2.1) with the
/// stencil the backend always has -- `ContextAttributes::default()` in Rust.
export function op_webgl_get_context_attributes(canvasId) {
  const recorded = contextAttributes.get(smiU32(canvasId, "canvas_id"));
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
  const canvas = smiU32(canvasId, "canvas_id");
  const error = smiU32(code, "code");
  const valid =
    error === INVALID_ENUM || error === INVALID_VALUE || error === INVALID_OPERATION || error === OUT_OF_MEMORY;
  pushError(canvas, valid ? error : INVALID_OPERATION);
}

export function op_webgl_record_out_of_memory(canvasId) {
  pushError(smiU32(canvasId, "canvas_id"), OUT_OF_MEMORY);
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
 *
 * The arguments are converted all the same: deno_core converts them before the
 * Rust body runs, so one it refuses is a TypeError there whatever the answer.
 */
export function op_text_cache_peek_pin(
  text,
  fontRequest,
  fontSize,
  fontWeight,
  italic,
  fillColor,
  textAlign,
  textBaseline,
  canvasW,
  canvasH,
) {
  stringOf(text, "text");
  stringOf(fontRequest, "font_request");
  f32BitsOf(fontSize, "font_size");
  smiU32(fontWeight, "font_weight");
  toBool(italic, "italic");
  smiU32(fillColor, "fill_color");
  smiU8(textAlign, "text_align");
  smiU8(textBaseline, "text_baseline");
  smiU32(canvasW, "canvas_w");
  smiU32(canvasH, "canvas_h");
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

// ---- counters and hints -------------------------------------------------------

/**
 * `op_alloc_host_callback_id`: the next id a host callback registers under.
 *
 * The embedded op takes it from a per-session allocator that refuses to wrap --
 * a caller must fail to register rather than register under someone else's id --
 * and the ids are only ever compared, never sent anywhere that assigns meaning
 * to their value. So the producer keeps the same allocator here: its own
 * counter, exhausted rather than wrapped.
 */
export function op_alloc_host_callback_id() {
  if (nextCallbackId > MAX_CALLBACK_ID) {
    throw new RangeError("host callback ids are exhausted");
  }
  const id = nextCallbackId;
  nextCallbackId += 1;
  return id;
}

let nextCallbackId = 1;
// The op returns an `i32`, and a callback id that wrapped would be one handler
// answering for another's events.
const MAX_CALLBACK_ID = 0x7fff_ffff;

/**
 * `op_trigger_gc`: nothing, and that is the honest answer here.
 *
 * The embedded op asks V8 for a full collection through
 * `low_memory_notification`, which is an engine-private call. The JavaScript
 * engine this content runs in is WebKit's, in this process, and it exposes no
 * such call to a Worker -- there is nothing to forward the hint to, and a round
 * trip to the host would ask the wrong engine. Content's `triggerGC()` is a
 * hint in its own API too, so a hint nobody can act on is a hint dropped.
 */
export function op_trigger_gc() {}

/**
 * `op_create_image`: the id a new `Image` gets.
 *
 * A counter bump in process too -- the embedded op stopped asking the render
 * thread for one because a busy renderer blocked `new Image()` for hundreds of
 * milliseconds -- and the id means nothing until the image is loaded. The
 * producer allocates from its own range for the same reason it allocates GL
 * resource ids: the host binds whatever it is told.
 */
export function op_create_image() {
  const id = nextImageId;
  nextImageId += 1;
  return id >>> 0;
}

let nextImageId = 1;

// ---- text codecs ---------------------------------------------------------------
//
// A string into bytes and back takes no host resource, which is why the op
// boundary answers these here. The conversions are `text-codec.mjs`, a port of
// `shared::codec` held to it by `scripts/test-text-codec-agreement.sh` -- a game
// that writes a save file on one platform has to be able to read it on another.

/// `migo.encodeMultiFormats(text, coding)`.
export function op_encode_multi_formats(original, coding) {
  return encodeString(stringOf(original, "original"), stringOf(coding, "coding"));
}

/// `migo.decodeMultiFormats(bytes, coding)`.
export function op_decode_multi_formats(buffer, coding) {
  return decodeBytes(bytesOf(buffer, "buf"), stringOf(coding, "coding"));
}
