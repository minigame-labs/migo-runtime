// Which channel a frame packet leaves on.
//
// G0's P3 measured both and neither wins outright: a loopback WebSocket has a
// low fixed cost and grows with bytes, a custom-scheme request has a higher
// fixed cost and is nearly flat. So the transport is a hybrid, and the crossover
// is where latency and host CPU agree -- 64 KiB. The reasoning, the table and
// the records are in `MigoFrameChannelPolicy` on the host side.
//
// THE THRESHOLD IS NOT WRITTEN HERE. It arrives in the configuration the host
// injects, from `MigoFrameChannelPolicy.socketCeilingBytes`. A copy in this file
// would be a second source for a number that has one measurement behind it, and
// the two would drift the first time the measurement is redone on new hardware
// -- silently, because both halves would still be internally consistent. A gate
// comparing two constants was the alternative; one constant needs no gate.

export const CHANNEL_SOCKET = "loopback_websocket";
export const CHANNEL_SCHEME = "scheme_request";

/**
 * A sender that picks a channel per packet.
 *
 * @param {object} options
 * @param {(bytes: Uint8Array) => void} options.sendOverSocket  the open socket.
 * @param {string} options.schemeUrl  where a large packet is POSTed.
 * @param {number} options.socketCeilingBytes  the largest packet the socket
 *        carries, inclusive. From the host; see above.
 * @param {(error: Error, byteCount: number) => void} [options.onSchemeFailure]
 *        a large packet that did not make it. Reported rather than thrown: the
 *        send is asynchronous, so by the time it fails the caller that produced
 *        the frame has long returned, and there is nobody left to catch.
 * @returns {(bytes: Uint8Array) => string} the channel the packet went on.
 */
export function createHybridSender({
  sendOverSocket,
  schemeUrl,
  socketCeilingBytes,
  onSchemeFailure,
} = {}) {
  if (typeof sendOverSocket !== "function") {
    throw new TypeError("the hybrid sender needs a socket to fall back to");
  }
  if (typeof socketCeilingBytes !== "number" || !Number.isFinite(socketCeilingBytes)) {
    throw new TypeError(
      "the hybrid sender needs the host's socketCeilingBytes; it does not carry a copy",
    );
  }

  return function send(bytes) {
    if (bytes.byteLength <= socketCeilingBytes || typeof schemeUrl !== "string") {
      sendOverSocket(bytes);
      return CHANNEL_SOCKET;
    }
    // `fetch` rather than a synchronous XHR, and the difference matters: this
    // runs on the Worker that is about to build the next frame, and a
    // synchronous request would block it for the whole round trip. The verdict
    // comes back on the downlink either way, so nothing here waits for a reply.
    //
    // The body is a fresh view over the same buffer -- not a copy. `fetch`
    // reads it before returning to the event loop; a producer that recycles
    // frame buffers must not recycle this one until the promise settles, which
    // is the same rule the socket's `send` already imposes.
    fetch(schemeUrl, { method: "POST", body: bytes, cache: "no-store" })
      .then((response) => {
        if (!response.ok) {
          onSchemeFailure?.(
            new Error(`the host answered ${response.status} for a ${bytes.byteLength}-byte frame`),
            bytes.byteLength,
          );
        }
      })
      .catch((error) => onSchemeFailure?.(error, bytes.byteLength));
    return CHANNEL_SCHEME;
  };
}
