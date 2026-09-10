# Performance+ WebContent producer

Source for the bundle that runs inside WebKit's WebContent process. Bundled
and minified by `scripts/build-apple-sdk.sh` into
`../../Sources/MigoApplePerformancePlus/Resources/`.

🏁 **G0 has run (2026-09-10).** This file used to say it had not, while
`../../Sources/MigoApplePerformancePlus/README.md` said the topology was decided;
both were about the same choice, so one of them was wrong, and that contradiction
is what surfaced the adversarial review. Now they agree with the evidence in
`docs/performance/apple/g0/`.

**The agent is a Dedicated Worker and the transport is a hybrid that switches at
64 KiB** — loopback WebSocket below, custom-scheme request above. Measured on an
iPhone 12 / iOS 17.0.3, 200 samples per class, no errors: below 64 KiB the socket
is 1.9–2.2× faster and costs 4× less host CPU; at 1 MiB the scheme request is 4.4×
faster and costs 4.6× less. Latency and CPU put the crossover in the same place,
so the split is not a compromise between metrics that disagree — each arm wins
both on its own side. And 64 KiB is the class this project already calls "a
frame's worth of draw commands", so the rule reads as *commands over the socket,
textures over the scheme*.

**Worker rAF exists and does not need feature-detecting away**: `worker_raf` came
back available on the device, at both origins. It is still measured against the
Window relay and the host `CADisplayLink` relay, because which clock *wins* is
gate 2's question and not gate 1's — but the producer no longer has to be written
for a Worker that might not have it.

The producer still supports a Window agent, and the reason changed. It is no
longer that G0 might select one: the review of 2026-09-07 was right that
synchronous XHR gives a Window-free path to a blocking readback without
`SharedArrayBuffer`, and wrong that this makes the two comparable. Measured, a
synchronous XHR round trip costs **1.15 ms against the socket's 0.216** — 5.4× on
latency and 6.1× on host CPU, on every synchronous readback the engine performs.
Window stays supported as a fallback with a known price, not as a candidate.

When a Worker is selected, game JavaScript/Wasm runs there and the Window is a
relay. A small `SharedArrayBuffer` may be used only for same-WebContent
Worker↔Window synchronization (control/reply mailbox); it never carries frame
bytes. Frame bytes use a bounded transferable `ArrayBuffer` ping-pong/pool to
the Window before entering the transport selected by G0. When Window is
selected, there is no Worker relay.

The transport probe compares custom scheme, loopback WebSocket, and a hybrid
only after directional-bottleneck evidence. WebKit bug
[191362](https://bugs.webkit.org/show_bug.cgi?id=191362) is officially
**RESOLVED FIXED**, so it is not a pre-written POST-body failure. The probe
must exercise the actual payload/API/body type on each target OS/device and
record secure-context/isolation, CORS, copy count, body delivery,
cancellation, streaming, and backpressure.

This WebContent bundle does not make the current Performance+ skeleton
V8-free. The current dependency chain may still link V8; the future release
gate must prove the final Performance+ artifact and dependency closure do not.

Not a cross-platform adapter: bootstrap order, selected topology/transport, and
the Apple release receipt are part of this source's contract. Tests run under
node, with no device or simulator, against the same golden wire corpus used by
the Rust validator; device claims require the G0 evidence.
