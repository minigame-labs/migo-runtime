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

What separates a product from a skeleton is what has run, so that is what this
section reports -- per target, with where the running happens.

| | What has run, and where |
|---|---|
| `core/` (`MigoAppleCore`) | built, tested and cross-compiled for `aarch64-apple-ios` on every pull request |
| `Sources/MigoAppleRenderer` | iOS and simulator shipping slices and an isolated macOS external-frame diagnostic package, built by the Apple SDK workflow; its sync-barrier ABI tests run on the simulator |
| `Sources/MigoApplePerformancePlus` | its acceptance suite runs a real WebContent producer against a real renderer on the iOS simulator: content drawing through the engine's own WebGL and 2D facades, textures, text, images, touches, a frame larger than one packet, a synchronous readback of the frame content submitted, and a game playing its packaged sounds. `MigoGameView` -- the product surface -- runs an installed game there with nothing supplied by the test: its own session, layer and display clock turn the frames, the game's exit reaches the app, and the next game starts on the same layer |
| `Sources/MigoMacV8` | `MigoGameView` runs a game in an app signed with the hardened runtime and allow-jit (`scripts/test-macos-game-view.sh`, on the macOS CI leg): V8 with a JIT and WebAssembly, Canvas2D on ANGLE, frames on the display clock, signed content verified with its key and tampered content refused, and the view refusing to start without the entitlement |
| `Sources/MigoAppleWebKit` | a session with no content surface; not in the SDK (see below) |
| `WebContent/PerformancePlus` | every op it answers is checked against the engine's own ops -- the same facade calls run in both runtimes and the commands compared -- on every pull request, under node and Rust |
| `ProbeApp/` | sources exist; it answers G0 questions and is never linked into a product |
| a real iPhone | `MigoGameView` on an iPhone XS Max (iOS 18.7), 2026-09-25: a game installed, loaded, turning frames on the display clock, exiting to the app and the next game running on the same layer; the soft keyboard; signed content verified, updated and refused when tampered. And a game playing its packaged sounds through the host's audio (2026-09-23) -- which the simulator cannot answer, having no audio output there |

A release builds the three engine groups in Release, checks the iOS archives
contain no JavaScript engine and the macOS archive runs a game with a JIT,
assembles them, and publishes `migo-<version>-apple-sdk.zip` only after the
asset itself has been unpacked, built for iOS and macOS, and has run a game
(`scripts/test-apple-sdk-release-asset.sh`).

The Performance+ topology is selected, and the G0 probes that selected it are in
`ProbeApp`: content JavaScript runs in a Dedicated Worker, frames cross on a
custom scheme with a socket for small ones, and `SharedArrayBuffer` is not
available on that origin -- so the synchronous calls a blocked `readPixels`
needs travel as a request body instead, which is what `src/sync-call.mjs` and
the host's sync endpoint are.

## Integrating

A release publishes this package as `migo-<version>-apple-sdk.zip`. It unpacks
to `MigoApple/`; add that directory to Xcode with *File > Add Package
Dependencies > Add Local*, and link one product:

| App | Product | Import |
|---|---|---|
| iOS 15.2+ | `MigoApplePerformancePlus` | `import MigoApplePerformancePlus` |
| macOS 11+ | `MigoMacV8` | `import MigoMacV8` |

Both products have the same two calls. Install the game package that ships in
your app bundle (a directory with `game.js` at its root), then load it into a
`MigoGameView`:

```swift
// .unsigned for a package inside your own (already signed) app bundle;
// .verified(publicKey:) for packages you download -- see below.
let configuration = try MigoGameView.Configuration.standard(contentSigning: .unsigned)
try MigoGameInstaller.install(
    package: Bundle.main.url(forResource: "my-game", withExtension: nil)!,
    id: "my-game", version: appBuildNumber, into: configuration.directories)

let gameView = MigoGameView(configuration: configuration)
gameView.onEvent = { event in /* .ready, .exitRequested, .failed(reason), ... */ }
view.addSubview(gameView)          // any size; the game gets it at first layout
gameView.loadGame(id: "my-game")   // entry defaults to game.js
```

`version` makes relaunches free: the same version is not copied again.

`contentSigning` has no default. `.verified(publicKey:)` takes a 32-byte raw
Ed25519 public key; every package must then carry `manifest.json` (`{"version":
1, "entry": "game.js", "timestamp": <unix seconds>, "files": {"<path>":
"<sha256 hex>", ...}}`) and `manifest.sig`, the raw 64-byte signature of
`manifest.json`'s exact bytes. The engine verifies a package in full on its
first launch and refuses one that does not match.
The view owns everything else -- engine session, `CAMetalLayer`, display
clock, touches (and on macOS the mouse, wheel and keys), app lifecycle, the
soft keyboard, and on iOS the audio session and recovery from a WebContent
crash.

It also answers what content asks of the device. `setKeepScreenOn` holds the
display awake -- the idle timer on iOS, a power assertion on macOS -- and gives
the app's own setting back when the game ends. `getNetworkType` and
`onNetworkStatusChange` follow `NWPathMonitor` (a wired connection is `wifi`, a
cellular one `unknown`, as on Android). On iOS `vibrateShort`/`vibrateLong`
drive the Taptic Engine and `getBatteryInfo` reports the battery and Low Power
Mode; a Mac answers those two "not supported", as a device without the
hardware does. Entries a game writes with `getGameLogManager().log` arrive as
`onEvent(.gameLog(json))`, for the app to keep or upload.

Not offered on these lanes, and refused rather than faked:
`setDeviceOrientation` (the size is fixed at start), `setEnableDebug` (there is
no debug panel), `getHeapStatistics` on iOS (content runs in WebKit's
JavaScriptCore, which exposes no heap figures), and downloaded subpackages --
the engine refuses a dynamic install whenever signing is enforced, because a
downloaded subpackage carries no signature; subpackages inside the package work.

**iOS.** The game's size is fixed when it starts (a mini-game lays itself out
once), so lock the hosting controller to the game's orientation.
`MigoGameView.unavailabilityReason` says whether the device can run the lane.

**macOS.** Sign the app with the hardened runtime and
`com.apple.security.cs.allow-jit`; without it the view refuses to start and
`onEvent` says why (V8 would die at its first compile, and a jitless V8 has no
WebAssembly). Embed ANGLE in a Run Script build phase before signing -- see
*ANGLE dependencies and embedding* below -- and add
`@executable_path/../Frameworks` to the runtime search paths.
`Configuration.mouse` chooses whether the mouse is sent as a touch (phone
content, the default), as mouse events (PC content) or both.

`scripts/test-macos-game-view.sh` builds exactly this -- a sixty-line app in
`tests/swift_host/macos-game-view` -- signs it both ways and runs it.

## Three products, three JavaScript execution models

| Product | Where content JS runs | Renderer | In the SDK |
|---|---|---|---|
| `MigoApplePerformancePlus` | WebContent, in a Dedicated Worker | this process: Skia + ANGLE/Metal | yes, iOS: `MigoGameView` |
| `MigoMacV8` | this process, V8 with JIT | this process: Skia + ANGLE/Metal | yes, macOS: `MigoGameView` |
| `MigoAppleWebKit` | WebContent, full web platform | WebKit | no: a session with no content surface |

`MigoAppleWebKit` has its WebView session and per-game origin, and nothing for
content to call: running a game there needs `migo.*` implemented on the web
platform itself, which no repository has yet (`migo-web-adapter` goes the other
way, browser globals on top of `migo.*`). It is the lane for a device with no
JIT -- Lockdown Mode takes WebContent's -- and for a first App Review
submission, and it is built when one of those has a customer behind it.

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
      ProducerBundle/          generated: the WebContent bundle (gitignored)
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

`ProbeApp` never enters a release target. It answered G0 and stays to answer it
again when a new iOS version ships.

## Building

The shipping matrix has one native product per platform group:

| Platform group | Rust product | Archive features |
|---|---|---|
| iOS device | `performance-plus` | `--no-default-features --features external-frames` |
| iOS simulator | `performance-plus` | `--no-default-features --features external-frames` |
| macOS | `macos-v8` | default V8 features |

These groups share the `MigoEngine` C module and XCFramework because their
platforms are disjoint. A macOS Performance+ shipping build is rejected. The
WebKit and macOS V8 products do not yet implement application sessions;
compiling them does not establish that the corresponding runtime product is
complete.

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
