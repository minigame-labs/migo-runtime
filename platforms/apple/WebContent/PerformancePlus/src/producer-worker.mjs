// The producer: the agent content runs in, and the only one that talks to the
// host's frame channel.
//
// Why a Worker and not the Window is measured rather than stylistic -- a Worker
// has no `document`/`window`, which is what makes this environment match the
// five platforms that already ship, and its `[[CanBlock]]` is true, which every
// synchronous readback the engine supports needs. Both are recorded with their
// evidence in `../../../Sources/MigoApplePerformancePlus/README.md`.
//
// The content entry point is imported here rather than bundled with this file:
// content is the game's, this is the engine's, and one bundle holding both would
// mean an engine release could not be swapped under a game that already shipped.

import { constructOpError } from "./engine-core.mjs";
import { bindEngineHost, readEngineSessionConfig } from "./engine-host.mjs";
import { ServiceChannel } from "./service.mjs";
import { SyncCaller } from "./sync-call.mjs";
import { connectFrameSession } from "./worker-bootstrap.mjs";

// The Worker's own `postMessage`, taken before anything else can replace it.
// The engine's global scope installs a mini-game `postMessage` -- the open data
// context's, which is what content means by that name on every Migo platform --
// and from then on `self.postMessage` is the engine's function, not the channel
// to the page. Measured: a report sent through the global after the engine
// loaded went to the open data context and asked for an offscreen canvas.
const postToPage = self.postMessage.bind(self);

/** Tell the page, which relays to the host. */
function report(message) {
  postToPage(message);
}

let session = null;

self.onmessage = async (event) => {
  const message = event.data;
  if (!message || message.type !== "start") return;
  // One start per worker. A second would connect a second socket while the
  // first is still crediting frames, and the host cannot tell the two apart.
  self.onmessage = null;

  const config = message.config ?? {};

  // The engine session, read before connecting: the service stream is stamped
  // with its generation and shares the socket the connection opens.
  let identity;
  let services;
  if (config.engineSession !== undefined) {
    try {
      identity = readEngineSessionConfig(config.engineSession);
      if (typeof config.serviceUrl === "string") {
        services = new ServiceChannel({
          generation: identity.runtimeGeneration,
          socketCeilingBytes: config.socketCeilingBytes,
          serviceUrl: config.serviceUrl,
          replyUrl: config.replyUrl,
          // The class the engine registered under the name the host sent --
          // `StorageError`, `IOError` -- so content's `catch` sees what it
          // sees on every other platform.
          errorFor: constructOpError,
          onFailure: (error) => report({ type: "failed", stage: "services", detail: String(error) }),
        });
      }
    } catch (error) {
      report({ type: "failed", stage: "engine", detail: String(error && (error.stack || error)) });
      return;
    }
  }

  try {
    session = await connectFrameSession({
      url: config.frameChannelUrl,
      // Both from the host or neither: `schemeUrl` names the endpoint and
      // `socketCeilingBytes` is the measured threshold that decides when to use
      // it. The producer carries no copy of either.
      schemeUrl: config.frameSchemeUrl,
      socketCeilingBytes: config.socketCeilingBytes,
      onSchemeFailure: (error, byteCount) =>
        report({
          type: "failed",
          stage: "uplink-scheme",
          detail: `a ${byteCount}-byte frame did not reach the host: ${error}`,
        }),
      // Off unless the host asks. A verdict arrives per submitted frame, and
      // relaying one is a structured clone to the page plus a hop onto the
      // host's main thread -- 60 of each per second, on the latency path this
      // lane exists to shorten, to deliver a number `FrameSession` has already
      // applied. Diagnostics and tests turn it on; the product does not.
      onVerdict: config.reportVerdicts ? (verdict) => report({ type: "verdict", ...verdict }) : undefined,
      // Always reported: the host replaced the runtime under us, which is
      // terminal for this content and happens once.
      onGenerationLost: (generation) => report({ type: "generation-lost", generation }),
      services,
    });
  } catch (error) {
    report({ type: "failed", stage: "connect", detail: String(error) });
    return;
  }

  // Connected is worth saying on its own: it is the last milestone the engine
  // owns, so a failure after it is content's and a failure before it is ours.
  report({ type: "connected", credits: session.credits });

  // The synchronous barrier, when the host serves one. A blocking request from
  // this Worker rather than `Atomics.wait` on a shared record: the content
  // origin is a custom scheme, where WebKit provides no SharedArrayBuffer. The
  // URL is the host's for the same reason the socket's is -- this file does not
  // build endpoints.
  const sync =
    typeof config.syncCallUrl === "string" ? new SyncCaller({ url: config.syncCallUrl }) : undefined;

  // The engine's own API layer -- WebGL, Canvas2D, `migo.*` -- when the host
  // described the session it answers for. Loaded before content, as the
  // embedded runtime evaluates its extensions before a game's first line: content
  // finds `migo`, `requestAnimationFrame` and the canvases already there.
  if (identity !== undefined) {
    try {
      bindEngineHost({
        session,
        identity,
        socketCeilingBytes: config.socketCeilingBytes,
        sync,
        services,
        report,
      });
      await import("./engine/boot.mjs");
    } catch (error) {
      report({ type: "failed", stage: "engine", detail: String(error && (error.stack || error)) });
      return;
    }
    report({ type: "engine-ready" });
  }

  if (typeof config.contentEntry === "string") {
    try {
      const module = await import(config.contentEntry);
      if (typeof module.start !== "function") {
        report({
          type: "failed",
          stage: "content",
          detail: `${config.contentEntry} exports no start()`,
        });
        return;
      }
      // `start` returns once content is initialised. It is not the frame loop:
      // the loop is driven by clock ticks from the host, and a `start` that
      // never returned would be content that never reports itself ready.
      // `report` because content cannot use the global to reach the host once
      // the engine is loaded: that name is the engine's (see `postToPage`).
      await module.start({ session, sync, report });
    } catch (error) {
      report({ type: "failed", stage: "content", detail: String(error) });
      return;
    }
  }

  report({ type: "ready", credits: session.credits, generation: session.generation });
};
