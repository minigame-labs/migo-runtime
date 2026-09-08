# Migo Apple platform

SwiftPM package for iOS, iPadOS and macOS: three products (the WebKit
compatibility lane, the conditional Performance+ lane, and the macOS native V8
lane) over one shared renderer target.

The authoritative contracts are [`contracts/apple/`](../../contracts/apple): the
deployment floor with its per-lane minimums, and the profile-selection policy.
Each carries its reasoning inline and each has a gate that fails when a consumer
drifts from it.

The maintainer plan behind them is deliberately **not** in this repository --
`docs/` is gitignored, so a link into it resolves for nobody who clones this.
What matters for anyone reading the code is in the contracts above and in the
comments here.

## Status

Not a skeleton any more, and not a product either. What separates the two halves
is whether a compiler has seen it, so that is what this section reports.

| | Where it is checked |
|---|---|
| `core/` (`MigoAppleCore`) | built, tested and cross-compiled for `aarch64-apple-ios` on every pull request, on an Apple silicon runner |
| `Sources/MigoAppleRenderer` | the Apple SDK workflow builds iOS/simulator shipping slices and an isolated macOS external-frame diagnostic package; the macOS diagnostic is not a V8 product test |
| `Sources/MigoAppleWebKit`, `Sources/MigoMacV8` | placeholders; they compile, and they do nothing |
| `WebContent/PerformancePlus` | its encoder runs against the same golden corpus as the Rust one, under node, on every pull request |
| `ProbeApp/` | a README. No sources exist, and none should until there is a device to run them on |

Nothing here has run on an iPhone. The Performance+ topology -- agent,
transport, frame clock, host shape -- is unselected, and the G0 probe evidence
selects it; see the table below.

The product baseline also does not prove that Performance+ is V8-free. The
Cargo half of that claim is checked on Linux every pull request and the archive
half now reads a real symbol table produced by Apple's toolchain, but the claim
that belongs to a *release* is about a shipped artifact, and there is no Apple
release yet.

The first Mac task is the G0 probe contract. It must measure the unresolved
topology axes below on the current minimum OS and representative devices;
ProbeApp evidence, not this skeleton, selects the product choices.

| G0 axis | Candidates that must be measured |
|---|---|
| JavaScript agent | Window vs Dedicated Worker |
| WebKit-to-app transport | custom scheme vs loopback WebSocket vs hybrid; hybrid only after directional-bottleneck evidence |
| frame clock | Worker `requestAnimationFrame` when feature-detected vs Window rAF relay vs `CADisplayLink` relay |
| `WKWebView` host shape | attached visible vs transparent overlay vs 1×1 vs off-screen vs occluded |

Correctness and isolation precede measurements of input-to-present latency, p99
jitter, missed-vsync rate, CPU, memory, cancellation, backpressure, lifecycle,
and background/occlusion behavior. No candidate may be called a winner in
advance.

WebKit bug [191362](https://bugs.webkit.org/show_bug.cgi?id=191362) is
officially **RESOLVED FIXED** and is not a pre-written custom-scheme failure.
Probe the actual payload/API/body type on each target OS/device for
secure-context/isolation, CORS, copy count, POST body delivery, cancellation,
streaming, and backpressure.

If G0 selects a Dedicated Worker, `SharedArrayBuffer` is only a small
same-WebContent-process Worker↔Window synchronization mailbox; it never carries
frame bytes. Frame bytes first use a bounded transferable `ArrayBuffer`
ping-pong/pool to Window and then the selected app transport. If G0 selects
Window, there is no Worker relay.

## Three products, three JavaScript execution models

| Product | Where content JS runs | Renderer | Ships in v1 |
|---|---|---|---|
| `MigoAppleWebKit` | WebContent, full web platform | WebKit | yes |
| `MigoApplePerformancePlus` | WebContent, Window or Dedicated Worker selected by G0 | this process: Skia + ANGLE/Metal | conditional |
| `MigoMacV8` | this process, V8 with JIT | this process: Skia + ANGLE/Metal | yes, macOS only |

`MigoAppleRenderer` is shared by the two native-rendering lanes and is
deliberately **not** a product. `MigoAppleWebKit` deliberately does not depend
on it: a host that asked for the compatibility baseline should not be linking a
renderer it will never drive.

Native JSC is not in v1. Reopen it only if credible lightweight content
produces a measured low-memory use case; until then, `MigoAppleWebKit` remains
the compatibility baseline for that content. This is a future evidence gate,
not a permanent architectural veto.

## Why iOS needs a native surface at all

The earlier C ABI candidate declared no iOS surface descriptor, on the
assumption that iOS would be a `WKWebView` container where WebKit owns the
drawing surface. That assumption inherited a claim -- "iOS has no performance
path" -- which turned out to be about in-process engines only. WebKit's
WebContent process is spawned by the system with the JIT entitlement; the
boundary is drawn around the process, not around the engine. So content
JavaScript can have JIT *and* Migo can own the renderer, as long as a frame's
worth of drawing commands crosses one process boundary once per frame.

That is the Performance+ lane, and it needs a host-owned `CAMetalLayer`, which
is why `include/migo/platform/ios.h` exists.

## Layout

```
platforms/apple/
  core/                      the engine-free package: no binary target, no dependencies
    Package.swift              so it resolves and builds with nothing fetched and nothing built
    Sources/MigoAppleCore/     profile policy, deployment floors, lifecycle, permissions, metrics
    Tests/
  Package.swift              the shipping package; floor values derived from
                             contracts/apple/deployment-floor.json
  Frameworks/                generated, gitignored: MigoEngine.xcframework
                             (iOS external frames, macOS V8, with migo-build.json)
                             ANGLELib{EGL,GLESv2}-{ios,macos}.xcframework
    Scripts/                  generated macOS ANGLE embedding helper
  Sources/
    MigoAppleRenderer/       internal: CAMetalLayer, display link, surface attach
    MigoAppleWebKit/         lane 1
    MigoApplePerformancePlus/ lane 2: transport, FrameIngress bridge, host view
      Resources/               generated: the WebContent bundle (gitignored)
    MigoMacV8/               lane 3
  Tests/
  WebContent/PerformancePlus/ source of the bundle that runs inside WebContent
  ProbeApp/                  G0 probes only; never linked into a product
```

**Two packages, and the split is load-bearing.** The shipping package declares a
binary target for an xcframework that `scripts/build-apple-sdk.sh` produces, so
it does not resolve until that script has run on a Mac -- which meant, for the
whole life of the skeleton, that *no* Swift here was ever compiled, including
the files that mirror `contracts/apple/*.json` and whose gates compare only
their text. `core/` is the half that needs no engine, so it is built and tested
on every pull request. `scripts/test-apple-swift-core-engine-free.sh` keeps it
engine-free and `scripts/test-apple-shipping-package-contract.sh` keeps the
shipping package consuming the artifact it declares.

`WebContent/PerformancePlus` is Apple product source, not a cross-platform
adapter, so it does not live in `adapter/`: it depends on WebKit bootstrap
order, on the topology and transport selected by G0, and on the Apple release
receipt.

`ProbeApp` never enters a release target. It exists to answer G0 and to keep
answering it when a new iOS version ships.

## Building

The shipping matrix has one native product per platform group:

| Platform group | Rust product | Archive features |
|---|---|---|
| iOS device | `performance-plus` | `--no-default-features --features external-frames` |
| iOS simulator | `performance-plus` | `--no-default-features --features external-frames` |
| macOS | `macos-v8` | default V8 features |

These groups share the `MigoEngine` C module and XCFramework because their
platforms are disjoint. A macOS Performance+ shipping build is rejected. The
Swift product placeholders do not yet implement application sessions; compiling
them does not establish that the corresponding runtime product is complete.

Build on macOS with Xcode, Python 3.9 or newer, and the Rust targets reported by
`build-apple-sdk.sh --print-slices <platform>`. First install the pinned ANGLE
runtime archives; the SDK script never downloads ANGLE itself:

```sh
bash scripts/fetch-apple-angle.sh ios
bash scripts/fetch-apple-angle.sh ios-simulator
export MIGO_APPLE_BUILD_ROOT=/tmp/migo-apple-build
bash scripts/build-apple-sdk.sh --platform ios --configuration Release
bash scripts/build-apple-sdk.sh --platform ios-simulator --configuration Release
```

Both groups remain in `platforms/apple/Frameworks/MigoEngine.xcframework`.
Staging is keyed by **product/configuration/platform** under the build root.
Each invocation reassembles every staged shipping group of that configuration;
Debug and Release never mix. Use the same build root for consecutive builds.
A failed build or publication restores the previous generated Frameworks and
WebContent Resources together. Helper scripts and resources are staged before
publication; the isolated diagnostic package is replaced as a whole. Rebuild
all groups when the engine sources change; staging is a local
build cache, not a release provenance system.

`MigoEngine.xcframework/migo-build.json` records each group's product,
configuration, deployment target, archive hash and header hashes. Assembly
rejects changed archives, changed headers and incompatible header sets. The
receipt's `complete` field means that all three shipping groups are present;
an iOS-only artifact intentionally has `complete: false`.

After the real Apple V8 archives and their build prerequisites are available,
the macOS shipping group can be added to the same Release artifact:

```sh
bash scripts/fetch-apple-angle.sh macos
bash scripts/build-apple-sdk.sh --platform macos --product macos-v8 \
  --configuration Release --require-all-slices
```

`--require-all-slices` refuses an incomplete shipping matrix.
`--assemble-only` reuses staged groups, including groups downloaded from the
same CI run, without compiling Rust. The SDK workflow assembles the validated
iOS device and simulator groups into one artifact; it does not publish a
release or claim that a macOS V8 archive was built.

The macOS external-frame renderer/ABI tests use a separate diagnostic product:

```sh
bash scripts/build-apple-sdk.sh --platform macos \
  --product external-frames-diagnostic --configuration Debug
```

Its Swift package lives at
`$MIGO_APPLE_BUILD_ROOT/diagnostics/Debug/package`, and its receipt carries
`diagnostic: true`. It never replaces the shipping engine artifact. This test
package exercises the renderer with external frames; it cannot prove that the
macOS V8 dependency closure, startup or JIT entitlement works.

### ANGLE dependencies and embedding

The SDK assembler includes ANGLE for the installed platform groups and requires
both runtime libraries for every engine group. It uses the pinned recipe's
original layouts: adjacent `libEGL.framework` and `libGLESv2.framework` on iOS,
and adjacent `libEGL.dylib` and `libGLESv2.dylib` on macOS. Wrapping the macOS
dylibs in separate frameworks changes the directory where EGL searches for GLES
and breaks loading.

On iOS, `Package.swift` declares both framework binary targets as conditional
dependencies of `MigoAppleRenderer`. Both edges are required because EGL opens
GLES dynamically and the linker cannot infer that dependency. Xcode supplies
the app's framework embedding phase. The iOS simulator CI test now consumes
that package directly, without copying ANGLE into its built test output.
Local SwiftPM binary paths must exist even on a different destination, so a
macOS-only build also needs at least one installed iOS ANGLE group.

On macOS, the SDK includes the dylib XCFramework pair and an embedding helper;
automatic SwiftPM embedding of bare dylibs is not assumed. In the host app's
build phase, before signing the app, run:

```sh
bash "$MIGO_PACKAGE/Frameworks/Scripts/embed-apple-angle.sh" \
  --frameworks-dir "$MIGO_PACKAGE/Frameworks" \
  --destination "$TARGET_BUILD_DIR/$FRAMEWORKS_FOLDER_PATH" \
  --architectures "$ARCHS" \
  --sign "$EXPANDED_CODE_SIGN_IDENTITY"
```

`MIGO_PACKAGE` is the generated package directory. The helper validates that
both XCFrameworks have one macOS slice supporting the requested architectures,
copies the pair with its original names, and signs the copies with the supplied
host identity. Omit `--sign` for unsigned local test output. The app must include
`@executable_path/../Frameworks` in its runtime search paths. The macOS V8 app
also needs its own JIT entitlement and signing/notarization setup.

The command-line diagnostic test runner has no app embedding phase; CI uses the
same helper and gives that output directory to the runner through
`DYLD_LIBRARY_PATH`. This is diagnostic loader coverage, not a signed app
embedding or minimum-OS release test.

To build ANGLE from source instead of installing the pinned runtime archives:

```sh
bash scripts/build-angle-apple.sh --fetch
bash scripts/build-angle-apple.sh --platform ios
bash scripts/build-angle-apple.sh --platform ios-simulator
bash scripts/build-angle-apple.sh --platform macos
bash scripts/build-angle-apple.sh --xcframework
```

The source build requires macOS and depot_tools. XCFramework assembly from
already installed runtime archives requires Xcode, but no ANGLE source checkout.
The pin and GN arguments live in
`contracts/artifact-manifest/apple-angle.lock.json`; the engine script owns the
slice list. `scripts/test-apple-angle-recipe-contract.sh` checks their agreement.

Host checks are available without Xcode:

```sh
bash scripts/test-apple-shipping-package-contract.sh
bash scripts/test-apple-sdk-packaging.sh
```

The packaging tests use clearly marked temporary toolchain fixtures. They test
slice preservation, product isolation, dependency closure and failure behavior;
real Mach-O validity, Xcode embedding, code signing and runtime startup still
require the macOS/iOS lanes and release evidence.
