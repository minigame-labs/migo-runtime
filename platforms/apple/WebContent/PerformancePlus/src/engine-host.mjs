// What the engine's ops need to know about the session they answer for.
//
// The op implementations in `lane-*.mjs` are module functions with deno_core's
// signatures -- `op_now(buffer)`, `op_frame_end_unified()` -- because the engine
// calls them exactly as it calls the Rust ones. What deno_core hands a Rust op as
// `OpState` they read from here: the frame channel, the session's identity, the
// surface's size. `producer-worker.mjs` binds it once, before the engine's modules
// are imported, and nothing binds it again: a second binding would be a second
// session answering the same engine.

import { platform } from "./engine-core.mjs";
import { FrameSession } from "./frame-session.mjs";

let bound = null;

function bigintField(config, name) {
  const value = config[name];
  if (typeof value !== "string" || !/^(0x[0-9a-fA-F]+|[0-9]+)$/.test(value)) {
    throw new TypeError(`the engine session's ${name} is a decimal or 0x-hex string, not ${typeof value}`);
  }
  return BigInt(value);
}

function sizeField(config, name) {
  const value = config[name];
  if (!Number.isInteger(value) || value <= 0 || value > 0xffff) {
    throw new TypeError(`the engine session's ${name} is a positive integer pixel count`);
  }
  return value;
}

/**
 * Parse the host-injected session description. Strings for the 64- and
 * 128-bit fields, because JSON numbers are doubles and these are identities.
 */
export function readEngineSessionConfig(config) {
  if (config === null || typeof config !== "object") {
    throw new TypeError("the engine session description is an object");
  }
  return Object.freeze({
    launchNonce: bigintField(config, "launchNonce"),
    runtimeGeneration: bigintField(config, "runtimeGeneration"),
    surfaceGeneration: bigintField(config, "surfaceGeneration"),
    resourceEpoch: bigintField(config, "resourceEpoch"),
    surfaceWidth: sizeField(config, "surfaceWidth"),
    surfaceHeight: sizeField(config, "surfaceHeight"),
    device: deviceProfile(config.device),
  });
}

/**
 * What the host knows about the device, for the `migo.*` calls a game makes
 * before its first frame: the screen, the pixel ratio, the safe area, the model.
 *
 * Handed over rather than asked for, because every one of these calls is
 * synchronous: a game reads `getWindowInfo()` to lay itself out, and a round
 * trip per call would be a blocked Worker for an answer the host already has.
 * The host hands it over at startup and again whenever the view the game is
 * shown in changes size or turns (`applyResize`).
 *
 * Absent when the host described none, and then the calls answer the way they do
 * on a platform with no device services: a failure that says so, rather than a
 * plausible screen size nobody measured.
 */
function deviceProfile(described) {
  if (described === undefined || described === null) return null;
  if (typeof described !== "object") {
    throw new TypeError("the device profile is an object when it is given at all");
  }
  // Frozen JSON strings, not objects: every one of these ops answers with the
  // JSON the engine's JavaScript parses, and parsing it here to stringify it
  // again would be work per call for the same bytes.
  const profile = {};
  for (const key of ["windowInfo", "deviceInfo", "menuButtonRect"]) {
    const value = described[key];
    if (value === undefined || value === null) continue;
    if (typeof value !== "string") {
      throw new TypeError(`the device profile's ${key} is the JSON the host hands over, as a string`);
    }
    profile[key] = value;
  }
  // A host that draws no menu button describes none, and gets the answer the
  // engine gives every such host (the default of the native system-info service):
  // an empty rect at the top right of the safe area, so a layout that keeps clear
  // of the button keeps clear of nothing, at the corner it would have been in.
  // Derived once per description, from the window the host described.
  if (profile.menuButtonRect === undefined && profile.windowInfo !== undefined) {
    const rect = emptyMenuButtonRect(profile.windowInfo);
    if (rect !== null) profile.menuButtonRect = rect;
  }
  return Object.freeze(profile);
}

/**
 * The empty menu button rect for the window `windowInfo` describes, as JSON: at
 * its safe area's top right edge (`safe_area` holds insets from the window's
 * edges). `null` when the window does not say its width and those insets -- the
 * native service fails the same call then, and so does this lane.
 */
function emptyMenuButtonRect(windowInfo) {
  let window;
  try {
    window = JSON.parse(windowInfo);
  } catch {
    return null;
  }
  const safe = window?.safe_area;
  if (!Number.isFinite(window?.window_width) || !Number.isFinite(safe?.top) || !Number.isFinite(safe?.right)) {
    return null;
  }
  const right = window.window_width - safe.right;
  return JSON.stringify({ width: 0, height: 0, top: safe.top, bottom: safe.top, left: right, right });
}

/**
 * Bind the session the engine's ops answer for. Once.
 *
 * @param {object} options
 * @param {FrameSession} options.session the frame channel.
 * @param {object} options.identity from `readEngineSessionConfig`.
 * @param {number} options.socketCeilingBytes the host's uplink threshold.
 * @param {object} [options.sync] the synchronous caller, when the host serves one.
 * @param {import("./service.mjs").ServiceChannel} [options.services] the service
 *   stream, when the host serves one.
 * @param {(message: object) => void} options.report posts a message to the host,
 *   through the page; nothing waits for it.
 * @param {MessagePort} [options.workerRelayPort] the page's half of content's `migo.createWorker`, when the host
 *   transferred one (see `page-entry.mjs`, `worker-page-relay.mjs`).
 */
export function bindEngineHost({ session, identity, socketCeilingBytes, sync, services, report, workerRelayPort }) {
  if (bound !== null) throw new Error("the engine host is already bound");
  if (!(session instanceof FrameSession)) throw new TypeError("the engine host needs a FrameSession");
  if (typeof socketCeilingBytes !== "number" || !Number.isFinite(socketCeilingBytes)) {
    throw new TypeError("the engine host needs the host's socketCeilingBytes");
  }
  if (typeof report !== "function") throw new TypeError("the engine host needs a report function");
  bound = Object.freeze({
    session,
    sync,
    services,
    report,
    workerRelayPort,
    socketCeilingBytes,
    launchNonce: identity.launchNonce,
    runtimeGeneration: identity.runtimeGeneration,
    // Mutable state the host changes by event: kept in one object so every
    // reader sees the same current values.
    state: {
      device: identity.device,
      surfaceGeneration: identity.surfaceGeneration,
      resourceEpoch: identity.resourceEpoch,
      surfaceWidth: identity.surfaceWidth,
      surfaceHeight: identity.surfaceHeight,
      backgrounded: false,
      contextLost: false,
    },
    // Nothing here: `op_now` counts from the producer's start (`platform.timelineNow`), the same origin the
    // frame-clock timestamps are put on, so `performance.now()` and `requestAnimationFrame`'s argument are one
    // timeline. The Rust op measures from the runtime's start, which is the same moment for the embedded runtime.
  });
  // A context state that arrived between the frame channel opening and this
  // binding: nothing could act on it then, and the epoch in it is still the one
  // the host expects.
  if (earlyContextState !== null) applyContextState(earlyContextState);
  earlyContextState = null;
  // Likewise a resize: the view was laid out again before the engine loaded.
  if (earlyResize !== null) applyResize(earlyResize);
  earlyResize = null;
  return bound;
}

/** The newest resize that arrived before the engine host was bound. */
let earlyResize = null;
/** The engine's surface-change ingress, from the host bridge. */
let resizeHook = null;

/**
 * Take the engine's surface-change ingress from the host bridge
 * (`_internalTriggerWindowResize`, which the embedded host calls when its
 * surface changes size). Call where the host events are bound.
 */
export function bindResizeEvents(bridge) {
  const hook = bridge?._internalTriggerWindowResize;
  resizeHook = typeof hook === "function" ? hook : null;
}

/**
 * Apply the host's resize: the view the game is shown in changed size or
 * turned. The engine's surface has already been resized to the same pixels
 * (`MigoEngineSession.resize(to:)`); this adopts that size and the device the
 * host now describes, then calls the engine's surface-change ingress, as the
 * embedded host does -- the main canvas follows the surface unless content
 * sized it, and content hears `onWindowResize` with what `getWindowInfo()` now
 * answers. State first, because the listeners read it.
 *
 * The ingress runs for a new surface size only, as the embedded host calls it
 * only when its surface changes: a device that changed alone -- the safe area,
 * the status bar -- is what `getWindowInfo()` answers next, and no resize.
 */
export function applyResize(update) {
  if (update === null || typeof update !== "object") {
    throw new TypeError("a resize is an object");
  }
  const surfaceWidth = sizeField(update, "surfaceWidth");
  const surfaceHeight = sizeField(update, "surfaceHeight");
  const device = deviceProfile(update.device);
  if (bound === null) {
    earlyResize = update;
    return;
  }
  const { state } = bound;
  const resized = surfaceWidth !== state.surfaceWidth || surfaceHeight !== state.surfaceHeight;
  state.surfaceWidth = surfaceWidth;
  state.surfaceHeight = surfaceHeight;
  state.device = device;
  if (!resized) return;
  try {
    resizeHook?.();
  } catch {
    // Discarded, as the embedded binding discards a hook's exception.
  }
}

/** The newest context state that arrived before the engine host was bound. */
let earlyContextState = null;
/** The engine's `webglcontext{lost,restored}` dispatcher, from the host bridge. */
let contextEventHook = null;

/**
 * Take the engine's context-event dispatcher from the host bridge. Call where
 * the host events are bound: the engine has loaded and content has not run.
 */
export function bindContextEvents(bridge) {
  const hook = bridge?._internalTriggerWebglContextEvent;
  contextEventHook = typeof hook === "function" ? hook : null;
}

function dispatchContextEvent(type) {
  try {
    contextEventHook?.(type);
  } catch {
    // Discarded, as the embedded binding discards a hook's exception.
  }
}

/**
 * Apply the host's `DOWN_CONTEXT_STATE`: adopt the resource epoch packets must
 * now name, and tell content what happened -- the embedded host's
 * `reconcile_context_lost`, applied here because content runs here.
 *
 * A level with an epoch, so it reconciles rather than replays: `lost` dispatches
 * `webglcontextlost` unless content was already told, recovery dispatches
 * `webglcontextrestored` if it was; and a newer epoch arriving with the context
 * not lost, when content was never told of a loss, is a whole loss and recovery
 * missed in between -- both are played, lost first, so content rebuilds against
 * the new table. `contextLost` is set before the event, because content's
 * listener reads `isContextLost()`.
 */
export function applyContextState({ lost, resourceEpoch }) {
  if (bound === null) {
    earlyContextState = { lost, resourceEpoch };
    return;
  }
  const { state } = bound;
  const epoch = BigInt(resourceEpoch);
  const advanced = epoch > state.resourceEpoch;
  if (epoch > state.resourceEpoch) state.resourceEpoch = epoch;
  if (lost) {
    if (!state.contextLost) {
      state.contextLost = true;
      dispatchContextEvent("webglcontextlost");
    }
  } else if (state.contextLost) {
    state.contextLost = false;
    dispatchContextEvent("webglcontextrestored");
  } else if (advanced) {
    dispatchContextEvent("webglcontextlost");
    dispatchContextEvent("webglcontextrestored");
  }
}

/** The bound host, or a throw naming the ordering mistake that got here. */
export function engineHost() {
  if (bound === null) {
    throw new Error("an engine op ran before producer-worker.mjs bound the engine host");
  }
  return bound;
}
