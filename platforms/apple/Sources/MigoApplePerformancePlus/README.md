# MigoApplePerformancePlus

Conditional native-rendering lane. Content JavaScript and WebAssembly remain
in WebKit's WebContent process while Migo owns the bounded, validated render
ingress. The shipping build is iOS/simulator only and selects the engine-free
`external-frames` Rust feature set. Its staged and assembled archive identity is
recorded by the SDK packager; the dependency and symbol gates still need to be
applied to release bytes before making a release-level no-V8 claim. macOS
external-frame builds are isolated renderer diagnostics, not this product.

## 🏁 The gate has run (2026-09-10). What the challenge reopened is closed again, on measurement.

The adversarial review of 2026-09-07 reopened two axes on one observation: synchronous
XHR is a second public blocking primitive, so a synchronous readback might be expressible
without `SharedArrayBuffer`, which would turn a capability fact into a performance
trade-off. It said a probe is entitled to decide a trade-off. The probe has run.

**iPhone 12 / iOS 17.0.3, release host, 200 samples per class, no errors** — records in
`docs/performance/apple/g0/{capability,transport}/`:

| the question this file left open | what the device answered |
|---|---|
| can a Worker set `responseType` on a synchronous XHR, so a readback needs no SAB? | **Yes** — every payload class from 64 B to 1 MiB completed. The capability claim is withdrawn as the challenge predicted. |
| what does that cost? | **~1.15 ms per call**, against 0.216 ms for a loopback WebSocket round trip: **5.4× the latency and 6.1× the host CPU**. A blocking readback is expressible and expensive. |
| is the custom scheme a secure context, without private API? | **Yes** — `secure_context` is available. This file's claim that promoting one "needs private API" was wrong. |
| does `crossOriginIsolated` work there? | **No.** COOP/COEP are served and not honoured, so the custom scheme has **no SharedArrayBuffer** — the conclusion survives its broken premise. |
| the third topology (content Worker blocks, I/O Worker owns the socket) | Included in the candidate space, and it is **exactly what the gate eliminated**: 25 of 520 candidates, all of them needing SAB the custom scheme has not. |
| which transport? | **Hybrid, switching at 64 KiB.** Loopback socket below (1.9–2.2× faster, 4× less host CPU), custom-scheme request above (4.4× faster, 4.6× less CPU at 1 MiB). Latency and CPU put the crossover in the same place, so it is not a compromise between disagreeing metrics. |

**So: content JavaScript in a Dedicated Worker, host-driven frame clock, hybrid transport
switching at 64 KiB.** Worker was already leading for a reason the challenge never touched —
a Worker has no `document`/`window`, which is what makes the environment match the other five
platforms instead of requiring DOM removal — and the challenge's own conclusion, that Window
is merely *worse* rather than impossible, is now quantified: 5.4× on every synchronous
readback.

The verdict is `provisional`, which is the coverage rule's answer to one device and one OS
and not a doubt about these numbers. `contracts/apple/capability-probe.schema.json` wants a
device on 15.0, 15.1 and 15.2 as well.

The section below is kept as written, because what it reopened and why is worth reading
next to what settled it.

## What is decided, what is leading, and what an adversarial review has reopened

Two of the three axes below were recorded here as *settled on capability grounds, not
performance grounds*, with the argument that a probe measuring latency cannot overturn a
constraint that leaves one candidate unable to express an operation at all. That argument
is sound in form. **Its premise has been challenged, and until the challenge is checked
these are candidates, not decisions.**

The challenge (adversarial design review, 2026-09-07) is a single observation: `Atomics.wait`
is **not the only public blocking primitive**. **Synchronous XHR** blocks too, and it is
available in a Worker; the Window-specific restriction is that a synchronous XHR may not set
a nonempty `responseType`. If that holds on the supported releases, then a synchronous
readback *can* be expressed without `SharedArrayBuffer` -- and the two axes below stop being
capability facts and become performance and complexity trade-offs, which a probe *is*
entitled to decide.

NOTE: This file previously said the topology was decided while
`../../WebContent/PerformancePlus/README.md` said G0 had not selected one. Both are about the
same choice, so one of them was wrong; that contradiction is what surfaced the challenge.
Neither may claim a decision until the capability gate produces an artifact.

**Content JavaScript in a Dedicated Worker -- leading candidate, no longer excluded on
capability grounds.** A Window agent's `[[CanBlock]]` is false, so `Atomics.wait` is
unavailable there, and every synchronous GPU readback this engine must support --
`getImageData`, `readPixels`, `toDataURL` -- has no *SAB* primitive to build on in a Window
agent. A Dedicated Worker's `[[CanBlock]]` is true. A Worker also has no `document`/`window`
to start with, which is what makes the environment match the other five platforms instead of
requiring DOM removal -- and that reason is unaffected by the challenge, which is why Worker
stays *leading*. What is withdrawn is only the claim that Window makes the operation
*impossible*: with synchronous XHR it may merely be worse.

Note also that a Worker does not need the Window as a frame relay: `WebSocket` is exposed to
Workers. The relay exists because a Worker blocked in `Atomics.wait` cannot service its own
socket -- which a **two-Worker** split answers directly (content Worker blocks, I/O Worker
owns the socket and notifies). That is a third topology this file did not consider and the
capability gate must include.

**The page origin `127.0.0.1` -- candidate, and one stated reason is contradicted.** This
file said a custom URL scheme "is not a secure context, and promoting one to a secure
context needs private API". Secure Contexts permits *user-agent-designated* trustworthy
schemes, and current WebKit is reported to treat scheme-handler schemes as potentially
trustworthy -- so no private API would be involved. Whether `crossOriginIsolated` actually
works for such a scheme on every supported iOS 15.x build is an empirical question, and it
is the one to settle. `127.0.0.0/8` being potentially trustworthy is independently true, and
the literal address is still required because a `localhost` *hostname* does not resolve
inside `WKWebView`. Safari 15.2 is when WebKit announced COOP/COEP and SAB, which is where
the 15.2 admission floor came from -- so if the origin question moves, that floor moves too.

**The frame clock is driven by the host through the control channel.** This one is not
challenged, and its reason is structural rather than capability-based: `requestAnimationFrame`
in this engine is already host-fed on every shipped platform --
`engine/crates/runtime-v8/src/rendering/webgl/03_raf.js` awaits `op_await_next_frame()`,
which the host's vsync resolves. Host-driven is therefore the isomorphic choice and leaves
the embedded JavaScript unchanged; only the op's landing point moves. A WebContent-side clock
would introduce a second timeline, and the phase error between the two is a problem this
topology does not otherwise have.

## What the host shape must satisfy, and what is still measured


Decided, because the OS enforces it:

- The `WKWebView` **must be attached to the view hierarchy**. Since iOS 16 an
  unattached one is killed.
- It must **not** be `isHidden`, zero-sized, or unattached.
- **Occlusion stops JavaScript execution**, so "covered by the CAMetalLayer" is
  not a usable shape. The target shape is attached and moved outside the visible
  area.

Still measured on the supported OS/device matrix, because these are
throttling-and-throughput questions rather than capability ones:

- Which attached-but-not-visible variant wins: off-screen, a 1x1 visible corner,
  or fully occluded. Whether the Worker gets throttled is what is being compared.
- Whether the loopback downlink needs a third leg at all. A custom-scheme
  streaming response runs its handler in the host process and saves a
  NetworkProcess hop, but it adds cross-origin and COEP complexity, so it is only
  worth building if a measurement shows the loopback downlink is the bottleneck.
- The one go/no-go: **per-frame IPC cost**. If it does not fit, this lane is not
  taken and the WebKit Full lane is the product.

The implementation must not infer any measured item from an old probe label or
from a simulator result. Correctness and isolation are measured before
performance, and a winner is judged on latency, p99 jitter, missed-vsync rate,
CPU, memory, backpressure, cancellation, lifecycle and occlusion/background
behaviour.

WebKit bug [191362](https://bugs.webkit.org/show_bug.cgi?id=191362) is
officially **RESOLVED FIXED**; it is not evidence that custom-scheme request
bodies fail. Any candidate still needs a real target-OS/device probe for its own
payload/API/body type, secure context, cross-origin isolation, CORS, copy count,
POST body delivery, cancellation, streaming and backpressure.

## Boundary and buffers

`SharedArrayBuffer` is allowed only as a small same-WebContent-process
synchronization mailbox for bounded control and reply state. It must never carry
frame bytes -- and it cannot: `WebSocket.send` takes no shared view, because
WebIDL gives `BufferSource` no `[AllowShared]`. Frame bytes move through a bounded
transferable `ArrayBuffer` ping-pong or pool and only then enter the app transport.

Which agent owns that transport is **open**, not the Window. `WebSocket` is exposed
to Workers, so the Worker can send its own frames; the Window relay exists only
because a Worker blocked in `Atomics.wait` cannot service its own socket. The
capability gate must compare at least three shapes -- Worker-direct, Worker to
Window relay, and a two-Worker split where a separate I/O Worker owns the socket
and notifies the blocked content Worker -- because the relay is a consequence of
the blocking primitive, and the blocking primitive is itself now a candidate
rather than a decision (see above).

The host side validates generation, sequence, lengths, credits and integrity
before materialization or GPU effects. Queues and allocations are bounded.

`webViewWebContentProcessDidTerminate:` must be implemented: retire the
generation, drop unacknowledged packets, rebuild the transport, and resume from a
signed checkpoint. **That recovery path has to carry `Canvas2DState` too.** Called
out because it is not hypothetical -- the same shape of defect, state that must
survive a context or process rebuild and was missed on one path, has broken four
times in this repository already (#48, PR #18, PR #19, PR #21).

The authoritative contracts are [`contracts/apple/`](../../../../contracts/apple): the
deployment floor with its per-lane minimums, and the profile-selection policy.
Each carries its reasoning inline and each has a gate that fails when a consumer
drifts from it.

The maintainer plan behind them is deliberately **not** in this repository --
`docs/` is gitignored, so a link into it resolves for nobody who clones this.
Everything above is either a specification fact, an OS-enforced constraint, or
checkable against this repository's own source, so none of it depends on a
document a cloner cannot read.
