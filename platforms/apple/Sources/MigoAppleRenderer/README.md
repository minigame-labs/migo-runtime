# MigoAppleRenderer (internal)

Owns the single `CAMetalLayer` a Session presents to, the display link that
paces it, and the Surface attach/update/retire handshake against the engine's C
ABI.

Not a product, and not a supported dependency: it is shared by
`MigoApplePerformancePlus` and `MigoMacV8`, and depending on it directly would
let a host acquire the renderer without the lane that knows how to drive it.

Rules that are not negotiable here, and the reason each one exists:

- **One layer per Session.** Offscreen canvases render into FBOs and are
  composited; they do not each get a layer. A layer per canvas multiplies
  drawables, caches, contexts and compositor memory.
- **Acquire the drawable last.** Everything that can be encoded before
  `nextDrawable` is encoded first, and the drawable is released as soon as the
  command buffer is committed. Holding one drains the pool and blocks the next
  `nextDrawable` call.
- **Never `waitUntilCompleted` on a foreground frame.** Synchronous readback
  goes through its own barrier with a deadline.
- **Carry Canvas2D state across every surface rebuild.** Backgrounding,
  rotation, a `contentsScale` change and a WebContent restart all rebuild the
  surface. This invariant has been broken four separate times in this repo
  (#48, PR#18, PR#19, PR#21); the fourth was an in-place resize, which is why
  the rule is phrased as "every rebuild path" and not "drop and recreate".

## What is here today

`MigoEngineCapabilities` — the preflight, and the first Swift in this repository
that links the engine rather than describing it.

It is one call to `migo_query_capabilities`, which takes no handle and can
therefore be asked before an engine, a session or a layer exists. It answers two
questions this target must not assume: whether the linked library accepts the
ABI version these sources were written against, and whether it advertises the
`CAMetalLayer` surface kind this target presents into.

Asking matters because the headers cannot answer. They declare every platform's
descriptors on every platform, and `MIGO_C_ABI_HAS_RUNTIME` reports the platform
the *host* compiled on. This repository has shipped the gap between the two:
windows-sdk-0.1.0 declared the Win32 descriptors, pinned their layout with C
assertions, exported every entry point, loaded, and advertised no attachable
platform kind at all, because the Rust half did not exist.

Apple now selects its own platform module. It advertises the iOS or macOS
`CAMetalLayer` kind for the target being built, and the preflight compares that
actual capability with the host's expected kind. The simulator and isolated
macOS diagnostic tests exercise the linked ABI; real-device presentation and
release validation remain separate evidence.

The dependency also does something no Swift here did before: it makes
`.github/workflows/apple-sdk.yml` check the artifact it builds. While no target
named the `MigoEngine` binary target, SwiftPM never had to resolve a slice, so
`swift build` passed on an xcframework it never opened.
`scripts/test-apple-shipping-package-contract.sh` keeps that from silently
reverting.

The shipping `MigoEngine` XCFramework selects external frames on iOS and V8 on
macOS. The macOS external-frame tests use an isolated diagnostic package. The
renderer declares both iOS ANGLE framework dependencies; macOS consumers embed
the adjacent dylib pair using the generated package helper. Neither native
dependency enters the WebKit compatibility lane.
