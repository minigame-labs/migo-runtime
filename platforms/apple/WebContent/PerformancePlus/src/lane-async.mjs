// Ops that return a promise the host settles.

import { engineHost } from "./engine-host.mjs";
import { drained } from "./engine-frames.mjs";

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
