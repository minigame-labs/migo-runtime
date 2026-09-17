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
  });
}

/**
 * Bind the session the engine's ops answer for. Once.
 *
 * @param {object} options
 * @param {FrameSession} options.session the frame channel.
 * @param {object} options.identity from `readEngineSessionConfig`.
 * @param {number} options.socketCeilingBytes the host's uplink threshold.
 * @param {object} [options.sync] the synchronous caller, when the host serves one.
 */
export function bindEngineHost({ session, identity, socketCeilingBytes, sync }) {
  if (bound !== null) throw new Error("the engine host is already bound");
  if (!(session instanceof FrameSession)) throw new TypeError("the engine host needs a FrameSession");
  if (typeof socketCeilingBytes !== "number" || !Number.isFinite(socketCeilingBytes)) {
    throw new TypeError("the engine host needs the host's socketCeilingBytes");
  }
  bound = Object.freeze({
    session,
    sync,
    socketCeilingBytes,
    launchNonce: identity.launchNonce,
    runtimeGeneration: identity.runtimeGeneration,
    // Mutable state the host changes by event: kept in one object so every
    // reader sees the same current values.
    state: {
      surfaceGeneration: identity.surfaceGeneration,
      resourceEpoch: identity.resourceEpoch,
      surfaceWidth: identity.surfaceWidth,
      surfaceHeight: identity.surfaceHeight,
      backgrounded: false,
      contextLost: false,
    },
    // Monotonic origin for `op_now`, taken when the session is bound: the Rust
    // op measures from the runtime's start the same way.
    startedAt: platform.now(),
  });
  return bound;
}

/** The bound host, or a throw naming the ordering mistake that got here. */
export function engineHost() {
  if (bound === null) {
    throw new Error("an engine op ran before producer-worker.mjs bound the engine host");
  }
  return bound;
}
