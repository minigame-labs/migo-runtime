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

import { connectFrameSession } from "./worker-bootstrap.mjs";

/** Tell the page, which relays to the host. */
function report(message) {
  self.postMessage(message);
}

let session = null;

self.onmessage = async (event) => {
  const message = event.data;
  if (!message || message.type !== "start") return;
  // One start per worker. A second would connect a second socket while the
  // first is still crediting frames, and the host cannot tell the two apart.
  self.onmessage = null;

  const config = message.config ?? {};
  try {
    session = await connectFrameSession({
      url: config.frameChannelUrl,
      // Off unless the host asks. A verdict arrives per submitted frame, and
      // relaying one is a structured clone to the page plus a hop onto the
      // host's main thread -- 60 of each per second, on the latency path this
      // lane exists to shorten, to deliver a number `FrameSession` has already
      // applied. Diagnostics and tests turn it on; the product does not.
      onVerdict: config.reportVerdicts ? (verdict) => report({ type: "verdict", ...verdict }) : undefined,
      // Always reported: the host replaced the runtime under us, which is
      // terminal for this content and happens once.
      onGenerationLost: (generation) => report({ type: "generation-lost", generation }),
    });
  } catch (error) {
    report({ type: "failed", stage: "connect", detail: String(error) });
    return;
  }

  // Connected is worth saying on its own: it is the last milestone the engine
  // owns, so a failure after it is content's and a failure before it is ours.
  report({ type: "connected", credits: session.credits });

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
      await module.start({ session });
    } catch (error) {
      report({ type: "failed", stage: "content", detail: String(error) });
      return;
    }
  }

  report({ type: "ready", credits: session.credits, generation: session.generation });
};
