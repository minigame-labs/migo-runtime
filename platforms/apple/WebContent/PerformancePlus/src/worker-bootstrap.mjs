// The twelve lines that put a real socket under `FrameSession`.
//
// Separated from the session for one reason: a `WebSocket` cannot be stood up in
// `node` without a dependency, and this directory ships into WebContent beside
// untrusted content where every dependency is one more thing inside that
// boundary. So the behaviour lives in `frame-session.mjs`, which is tested, and
// the socket lives here, which is checked end to end from the other side --
// `MigoFrameTransportTests` connects a real `URLSessionWebSocketTask` to the
// host's listener and asserts bytes survive both directions.
//
// The URL is the host's, handed to the Worker rather than built here: the port
// is ephemeral, and a producer that guessed it would be a producer that connects
// to whatever else is listening.

import { FrameSession } from "./frame-session.mjs";

/**
 * Connect to the host's frame channel.
 *
 * @param {object} options
 * @param {string} options.url  `ws://127.0.0.1:<port>/`, from the host. The
 *        literal address matters: `localhost` does not resolve inside
 *        `WKWebView`, which is measured rather than stylistic.
 * @param {number} [options.timeoutMillis] how long to wait for the socket.
 * @returns {Promise<FrameSession>} resolved once the socket is open, because a
 *          session handed back before then would accept a submit it could only
 *          drop.
 */
export function connectFrameSession({
  url,
  timeoutMillis = 10_000,
  onFrame,
  onVerdict,
  onGenerationLost,
} = {}) {
  return new Promise((resolve, reject) => {
    let socket;
    try {
      socket = new WebSocket(url);
    } catch (error) {
      reject(error);
      return;
    }
    // Binary, and set before the socket opens: the default is `blob`, and a
    // blob has to be read asynchronously -- which would put an await between a
    // frame-clock tick arriving and content seeing it.
    socket.binaryType = "arraybuffer";

    const session = new FrameSession({
      send: (bytes) => socket.send(bytes),
      onFrame,
      onVerdict,
      onGenerationLost,
    });

    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      socket.close();
      reject(new Error(`the host's frame channel at ${url} did not open in ${timeoutMillis} ms`));
    }, timeoutMillis);

    socket.addEventListener("open", () => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve(session);
    });
    socket.addEventListener("message", (event) => {
      session.handleMessage(new Uint8Array(event.data));
    });
    socket.addEventListener("close", () => {
      session.close();
    });
    socket.addEventListener("error", (event) => {
      session.close();
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      reject(new Error(`the host's frame channel at ${url} failed: ${event && event.type}`));
    });
  });
}
