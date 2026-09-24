# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- iOS and macOS SDK: `migo-<version>-apple-sdk.zip`, a Swift package with one
  view per platform. `MigoGameView` (`MigoApplePerformancePlus` on iOS,
  `MigoMacV8` on macOS) takes an installed game and owns everything between it
  and pixels -- engine session, `CAMetalLayer`, display clock, input, app
  lifecycle, and on iOS the audio session and WebContent crash recovery.
  `MigoGameInstaller` installs a package atomically and skips a version that is
  already there. Both products ship a privacy manifest that a gate checks
  against the engine's sources in both directions.
- iOS and macOS: the soft keyboard. `migo.showKeyboard` opens the system
  keyboard on iOS and a text field along the game's bottom edge on macOS; the
  player's text comes back as `onKeyboardInput`/`Confirm`/`Complete`. On the
  iOS lane the three keyboard ops became host commands to the same keyboard
  service the embedded runtime calls, so a host with no keyboard refuses them
  in the same words.
- C ABI: `MigoEngineConfig.code_signing_public_key`, the Ed25519 key signed
  content is verified against. Until now a C ABI host could not supply one, so
  its only configuration that loaded content was
  `MIGO_ENGINE_FLAG_ALLOW_UNSIGNED_CONTENT`. Appended to the record: a host
  passing the old size is zero-extended to "no key" and behaves as before.
  Setting the key and the unsigned flag together is refused.

- macOS: a host-owned `CAMetalLayer` can be attached through the C ABI.
  `MIGO_PLATFORM_MACOS_CA_METAL_LAYER` now appears in the library's advertised
  attachable kinds, and it appears because an attach ran, not because a backend
  compiled: `apple-sdk.yml` run 34017950809 built a real `CAMetalLayer` on a
  hosted macOS runner, carried it through `migo_session_attach_surface` as
  generation 1, and completed the whole retirement handshake — `begin_detach`,
  poll to `MIGO_SURFACE_RELEASE_RELEASED`, `migo_surface_release_destroy`,
  `migo_session_destroy`, `migo_engine_destroy` — before releasing the layer.
  An `NSView` is refused rather than resolved to a layer, so the host keeps
  ownership of its own drawable. The same run loaded the pinned ANGLE under the
  name the recipe declares and got an EGL display back from it. iOS is
  deliberately not included: its arm of the same module compiles everywhere and
  has attached nothing anywhere.
- Images with transparency are transcoded to ASTC at package ingest on devices
  whose GPU decodes it, and stay on ETC2 elsewhere. The block footprint is
  chosen per image by what it reconstructs: the encoder grades its own output
  against the source and takes the largest block whose worst channel error stays
  within budget, so a smooth image lands at 0.25 bytes per pixel — a quarter of
  ETC2 RGBA, at higher fidelity — while a sprite with a hard alpha edge stays at
  one byte per pixel rather than losing the edge. Opaque images stay on ETC2 RGB
  everywhere, because at half a byte per pixel it is smaller than any ASTC
  footprint. The choice is per image and per device, which is only possible
  because ingest runs on the device.
- Apple: ANGLE over Metal is built from source and pinned
  (`contracts/artifact-manifest/apple-angle.lock.json` +
  `scripts/build-angle-apple.sh` + `scripts/fetch-apple-angle.sh`) for every
  slice group the engine is built for — iOS, the iOS simulator and macOS, five
  configurations in all. ANGLE publishes no official prebuilt binaries for any
  platform, so these are self-hosted and hash-verified before use, the same
  model the Windows ANGLE runtime and the V8 archives already follow. It exists
  because there is no GL framework on iOS: rustc's own link line for the Apple
  slices asks for `-framework OpenGL` on macOS and for no GL framework at all on
  iOS, while Skia is configured for its GL backend. Metal is the only backend
  built; the desktop GL backend, which defaults to on for macOS, is pinned off
  so both Apple platforms resolve the same renderer. The published shape differs
  per platform because upstream's does — a framework bundle on iOS, where Apple
  accepts an embedded framework and rejects a bare dylib, and a shared library
  on macOS — and it is deliberately not evened out: `libEGL` locates `libGLESv2`
  at run time by composing a name against a directory, and that name and
  directory are platform-specific, so repackaging either side would leave ANGLE
  searching where its dispatch library is not. `--print-loader-layout` is what
  answers where each library has to sit; the xcframeworks are split along the
  same axis instead, one per library per platform family.
- The runtime reports how many times a frame's drawing crossed from JavaScript
  into native, on a five-second window, at `info` level: `[boundary] frames=…
  crossings/frame=… worst=… commands/frame=… commands/crossing=…`. The ratio is
  what a command stream exists to move, and it was previously only observable
  from inside a test.

### Changed
- Canvas2D commands cross the JavaScript/native boundary as a binary command
  stream rather than one op per call, sharing one buffer and one opcode space
  with the WebGL stream so the order between the two survives the crossing
  without a barrier between every pair of commands. A frame that issues three
  hundred 2D calls now crosses once.

### Fixed
- iOS: signed content was not verified. The Performance+ lane mounted the
  installed package as it was, so a host that configured a signing key got no
  verification at all. It now runs the embedded execution's launch sequence
  before anything is mounted -- sealed-receipt check, else a full manifest and
  file-hash verification that seals the tree -- and refuses the load on any
  mismatch, or on signing with no key.
- `MigoGameInstaller` could not update a game that had run under signing: the
  engine seals a verified tree read-only, and the installer replaced it in
  place. An update now renames the sealed tree aside, renames the new one in
  and removes the old one as a trusted uninstall.
- iOS: a game that called `migo.exitMiniProgram()` stopped drawing and the app
  was never told. The external-frame session ended on the request without the
  exit notification the in-process runtime sends; it now sends it.
- The Apple SDK could not put its iOS and macOS engines in one xcframework:
  assembly required byte-identical header directories, and each group's module
  map lists the frameworks its own archive links. Only the C headers are now
  compared across groups.
- SBOMs listed crates the shipped build never compiles. `cargo metadata`
  resolves features for the whole workspace, so a crate any member enabled was
  in every artifact's graph -- the iOS SDK, built without a JavaScript engine,
  would have listed `v8`. Each SBOM is now cut to its build's own `cargo tree`.
- ASTC 8x8 textures upload again. The engine mapped
  `VK_FORMAT_ASTC_8x8_UNORM_BLOCK` to `0x93B9`, which is the token for a 10x6
  block, so `glCompressedTexImage2D` rejected every such texture with
  `GL_INVALID_VALUE`. It had never fired because nothing produced ASTC 8x8, and
  the test that covered it asserted the same wrong number, copied from the
  constant it was checking; the tokens are now derived from the block size the
  extension assigns them to.
- `ctx.fillStyle = "transparent"` no longer paints opaque black. The keyword was
  missing from the engine's named-colour table, so it fell through to the
  unknown-name branch, which reads black -- the loudest possible wrong answer for
  a keyword whose meaning is "do not paint". `strokeStyle` and `shadowColor` had
  the same fault, and gradient colour stops were unaffected.

---

## v0.9.6 (2026-08-30)

A hotfix for a WebGL regression in v0.9.5.

### Fixed
- `bufferData(target, ArrayBuffer, usage)` uploads its data again. v0.9.5 added
  a `size < 0` guard to `bufferData` that ran before the code checked whether a
  payload was supplied, and the JS binding passes `size = -1` on the data path
  because the field is unused there. So the most common WebGL upload -- vertex
  and index buffers -- became a silent no-op that also recorded a spurious
  `INVALID_VALUE`, and every WebGL draw painted black. The op had no test;
  `migo-conformance`'s `webgl-basics` bundle caught it on the first run against
  the v0.9.5 release AAR. The negative-size check now runs only on the
  size-only form, and `op_buffer_data` has a regression test on both forms.

---

## v0.9.5 (2026-08-29)

A correctness and efficiency pass over io, graphics and audio. Redundant GL
calls that slipped past the state shadow are deduplicated and the shadow itself
stopped hashing to decide; `measureText` and per-sample `AudioParam` automation
stop allocating and re-walking on every call; `AnalyserNode`'s scalar
parameters take effect; `compressImage` shares a two-thread pool instead of
spawning one thread per image. A symlink race in sub-package and ZIP extraction
is closed, a WebGL context loss no longer strands a game between frames, and
`getUpdateManager()` stops inventing updates. Host frame-timestamp jitter no
longer drops frames for the rest of a session, and `MigoRuntime` reports the
real engine version.

### Added
- `AnalyserNode`'s scalar parameters take effect. `minDecibels`, `maxDecibels`
  and `smoothingTimeConstant` were accepted and then ignored, so
  `getByteFrequencyData` / `getFloatFrequencyData` returned unsmoothed data over
  a fixed range regardless of what content set. They now reach the audio thread
  through a dedicated op and apply.
- `scripts/build-aar.sh --jitless` builds an engine that asks V8 to stop
  generating machine code (`--jitless`). HarmonyOS 5.0.0(12) forbids a
  third-party VM from making memory executable, so Migo runs interpreted on
  NEXT whether it asks to or not; this is the build that produces the size of
  that penalty rather than guessing at it. Off by default, selected by no
  release path, and the artifact is named so it cannot be mistaken for one. The
  measured numbers are in `migo-bench/JITLESS.md`.

### Changed
- `migo.compressImage` no longer starts one OS thread per call. A batch — a
  screenshot sheet, an avatar pipeline — got a thread per image, each with a
  1 MB stack, all competing with the render thread. It now uses a shared,
  daemon pool of two.
- `PRESCREEN.md` and the prescreen report now say which published names are
  no-op stubs and which fail loudly, rather than covering all of them with one
  sentence true of only some. The two stubs that were failing silently now
  answer `"<api>:fail not supported"` like the other 86.
- On Android, `requestVsync` resolves its Java method ID once per process
  instead of hashing the method name and signature against the JNI method cache
  on every frame. The lookup is off the frame-scheduling path now; the id is a
  process-lifetime constant.
- The GitHub Release notes now lead with this file's `## v<version>` section.
  They previously carried only the asset table, verification steps, and GitHub's
  raw commit list — the curated record of what changed was in the repo but not
  on the release page. `write-release-notes.sh` refuses to write notes for a
  version whose section is missing or empty.
- Redundant GL calls stop reaching the driver on two more paths. `glScissor`
  could not be deduplicated before, because the dirty-region Canvas2D batcher
  and the DrawingBuffer blit both moved the scissor box outside the state
  shadow; both now route through it. The texture-unit, `glEnable`/`glDisable`
  and vertex-attribute shadows were rebuilt from hash maps and sets keyed by GL
  enum or `(vao, index)` into arrays and bitmasks, so deciding that a call is
  redundant no longer hashes.
- `AudioParam` automation is evaluated once per 128-sample block instead of once
  per sample. A parameter driven by `setValueAtTime`, a ramp, `setTargetAtTime`
  or `setValueCurveAtTime` walked its whole event timeline for every sample in
  the block; it now makes one forward pass.
- `measureText` and the Canvas2D font ops no longer allocate on every call. They
  cloned the whole text string to keep a character count for a render-thread
  timeout branch they rarely reach; they now take the count first and move the
  original.
- WebGL error reporting under queue pressure follows the spec. Past the
  per-context cap the queue used to discard the oldest un-retrieved error; it
  now keeps the oldest, drops the newest, reserves the last slot for a sticky
  `OUT_OF_MEMORY`, and counts the drops in the render diagnostics.
- `chacha20` (pulled in transitively by `rand`) is updated off a yanked
  release. No advisory attached; `cargo audit` stays clean.

### Fixed
- A re-linked WebGL program's uniforms reach the driver again. `glLinkProgram`
  gives a program fresh, zeroed uniform storage; the per-`(program, location)`
  dedup cache was not cleared across it, so content that re-linked a program and
  then re-uploaded an unchanged uniform value had that upload silently dropped —
  the draw used the reset value, with no GL error and nothing in a log.
- Sub-package and ZIP extraction is no longer open to a symlink race. An entry's
  path was validated as a string and then resolved again by a separate syscall;
  a path component could turn into a symlink in between and let the entry write
  outside its destination directory. Every component is now reached with
  `openat` under `O_NOFOLLOW` from a held directory descriptor, so a swapped
  component fails with `ELOOP` at the moment of use rather than after a passed
  check.
- `fetch` no longer puts the sandbox's internal path layout in a request to a
  remote server.
- Terminating a Worker while it is still starting up is no longer a silent
  no-op. The handle the host holds was published only after the worker runtime
  finished initialising, so `terminate()` called in that window did nothing and
  the worker ran to completion.
- A game is no longer stranded after a WebGL context loss and restore. The
  render drain dispatches `webglcontextlost` / `webglcontextrestored` into JS;
  a handler that resolves a promise or requests a frame leaves a pending task,
  and the event loop stayed parked on it until some unrelated host command
  arrived — which, for a game between frames, it never did.
- The C ABI's minimum `struct_size` for `MigoHostCallbacks` was wrong on ILP32.
  It used a literal `32`, correct for LP64's pointer width; a 32-bit host
  passing a struct that fully contained `dispatch` (minimum `20` there) was
  refused with `MIGO_ERROR_INVALID_ARGUMENT`. The minimum is now derived from
  the field offset.
- `migo.getUpdateManager()` no longer invents updates. It was deciding with
  `Math.random() < 0.3` at construction whether to fire
  `onCheckForUpdate({hasUpdate: true})`, so roughly a quarter of launches
  showed the game's own "new version — restart?" prompt and then `applyUpdate()`
  restarted nothing. This runtime has no update channel; the manager now says so
  instead of pretending.
- Non-Android hosts (Linux, Windows, OpenHarmony, bare C embedders) now remove
  a session's sandbox `/tmp` directory when the session ends. `GamePaths::clean_temp`
  had no caller outside Android's Java SDK, so every session left its
  `tmp/{id}` subtree under the cache root for the life of the install.
- Host timestamp jitter no longer drops frames for the rest of a session. The
  vsync decimator admitted a frame within a fixed 0.25 ms of its deadline, and
  every useful cadence puts that deadline exactly on a vsync — so a host whose
  frame-callback timestamps jittered by more than that dropped frames
  permanently: a replayed 60 Hz stream with 0.4 ms of jitter rendered 14 of 24
  vsyncs, a 60 fps request running at ~35 and staying there. The tolerance is
  now half the smaller of the last two delivered-vsync gaps, taking the frame
  nearest its deadline rather than the first past it. The frame rate the
  content asked for now also reaches Android's display-mode hint, so a request
  that is a whole divisor of the panel's rate gets an even cadence; and the
  requested-rate range and default, which four copies of the rule disagreed on
  (`2^31` and non-finite handled differently on each side, a 24 fps request
  silently raised to 30, a ceiling of 120 that left 144 Hz panels unreachable),
  now live in one module.
- `MigoRuntime.getNativeVersion()` and the public `MigoRuntime.SDK_VERSION`
  constant report the running engine's version. Both had frozen to an early
  release's number while the AAR's own `versionName` tracked `release/VERSION`,
  and since they were the same frozen string the SDK's skew check compared a
  value against itself and could never fire. The JNI `version()` now derives
  from `CARGO_PKG_VERSION` and `SDK_VERSION` from `BuildInfo.VERSION`, both of
  which `scripts/test-release-version-contract.sh` holds equal to
  `release/VERSION`.

---

## v0.9.4 (2026-08-24)

Android delivery gets more flexible — the engine can be kept out of the first
install and handed over at first launch — and Android cold start moves ahead of
the system WebView on every benchmark game. A sweep of the game-facing API by
behaviour rather than by reading turned up several that failed silently. The
`wx` namespace is gone.

### Added
- `MigoGameActivity.onCreateRuntimeConfig()`. A config handed to
  `buildLaunchIntent` travels through an in-process table keyed by a token in
  the intent, so it reaches the activity only when whatever started it was in
  the same process. A game opened from a deep link, a notification, a launcher
  shortcut or `am start` is not: the token is absent and the launch silently ran
  on a default config, whatever the host had configured everywhere else.
  Overriding this describes how the app runs games once, and it applies however
  the activity was reached. The default returns `null`, which is exactly the
  previous behaviour.
- `MigoNativeLoader.prepare(context, file)`, for hosts that deliver the engine
  themselves. It runs the same verification the load already does, on the
  thread the caller is on. It does not make the load safer -- what it changes
  is when a bad file is found: a truncated download or a mirror serving the
  previous release is otherwise discovered when a user opens a game, as a
  launch failure, rather than by the download code that still has the network
  connection. It also keeps the check off the main thread; verification is
  41 ms for a 45 MB release engine on a Mate 30 Pro, and the load afterwards
  finds the result recorded and hashes nothing.
- The first `readPixels` on a WebGL context no longer returns an empty buffer.
  While one canvas exists and nothing has read the default framebuffer, WebGL
  renders straight to the window surface and the intermediate DrawingBuffer is
  bypassed. The first readback ends that bypass -- a read needs a real FBO --
  and the engine bound the DrawingBuffer without putting anything in it, so the
  read returned `[0,0,0,0]` for pixels the game had just drawn. It happens once
  per context, at startup, which is the hardest kind of bug to notice and the
  easiest to blame on the content; content that builds textures by drawing and
  reading back gets one empty texture. `signal_default_fbo_readback` has
  documented this snapshot since the flag was introduced -- only the code was
  missing. Found by a new WebGL bundle in migo-conformance on its first run.
- Android hosts can keep the engine out of their first install. `libmigo.so`
  is ~17 MB of store download and ~45 MB installed per ABI, paid by every
  user whether or not they ever open a mini-game. Two new release assets let
  that cost move to the first game launch: `migo-<version>-android-nojni.aar`
  (the published AAR with `jni/**` deleted) and
  `migo-<version>-jni-android-<arch>.tar.gz` (the bytes it no longer carries).
  A host installs a `NativeLibraryProvider` through the new `MigoNativeLoader`
  and hands over the file; Migo verifies it against the artifact manifest the
  AAR already embeds before loading it, so a partial download or a mirror
  still serving the previous release fails with a readable reason instead of
  crashing inside the engine. Migo downloads nothing itself: on Google Play
  the only compliant source is Play Feature Delivery, and stores without it
  expect the host to serve the file, so one built-in downloader would be
  wrong for one of the two.
- `scripts/test-android-nojni-aar-contract.sh`, which holds the engine-less
  AAR to being a deletion rather than a second build -- identical
  `classes.jar`, identical embedded artifact identities, and every removed
  byte accounted for in exactly one engine archive.

### Changed
- The native library now loads on first use rather than in
  `MigoRuntime.getInstance()`. Every accessor that needs native calls loads
  first, so the packaged default behaves exactly as before; what changes is
  that merely obtaining the singleton no longer pulls the engine into the
  process.
- `LEGAL.md` states that hosting the engine binary to deliver it into your
  own app is covered by the Additional Use Grant and is not a Competitive
  Offering.
- Android cold start is materially faster, and now leads the system WebView on
  both metrics for all three benchmark games. Five costs sat on that path and
  none had to. Full-screen immersive mode was applied from `createSession` --
  after the window had been laid out with the system bars and the surface
  created at that smaller size -- so every launch resized the window and made
  the engine rebuild its GPU-side surface mid-startup; it is now applied in
  `MigoGameActivity.onCreate`, before the surface exists. And the session was
  created inside the `surfaceCreated` callback, holding the main thread for
  ~114 ms in the middle of the traversal that draws the window; it is now
  posted so the traversal can finish and draw first. The three engine-side
  costs are the entries below. Measured on a Mate 30 Pro against the device's
  Android System WebView, medians of 3 cold runs: first frame 385→274 ms
  (bunnymark), 384→258 (canvasmark), 523→344 (endless-runner); game-ready
  523→400, 378→336, 804→605. Steady-state fps and memory are unchanged.
- Session creation is ~38 ms cheaper. Building the Canvas2D text context
  costs 35-41 ms on an arm64 device -- `SkFontMgr_Android` parses
  `/system/etc/fonts.xml` and enumerates the system families, then the
  bundled fallback face is parsed on top -- and it was being done on the host
  thread inside `RenderThread::spawn`, before the render thread existed. Every
  session therefore delayed the start of EGL/Skia initialization by that much
  and then waited for it, whether or not the game ever drew a glyph. The
  context is now built by the render thread once its GPU capabilities are
  published: off the host's critical path, and still long before any game code
  can ask for a glyph, so no first `fillText` pays for it either. `Host::new`
  drops from 88-99 ms to 50-58 ms; game-ready improves by 16-53 ms across the
  three benchmark games.
- Creating a session no longer waits for the GPU. `Host::new` blocked on the
  render thread publishing its capabilities -- 30-44 ms with the caller's
  `createSession` blocked behind it -- although nothing between that point and
  the first line of launch JS reads them. The wait moved to just before any
  prelude or module runs, which is where the invariant it protects was always
  written down: no untrusted JS may observe the provisional all-false
  capability snapshot. A GPU failure is now reported from `startGame` rather
  than from `createSession`. Game-ready improves a further 10-21 ms.
- The dev player deploys a whole game bundle, not just `game.js` and
  `game.json`. It copied those two files by name, so it could not run any
  bundle carrying an asset -- a font, an image, a sub-package -- which made a
  whole category of conformance test impossible to write.
- The GLES dispatch table is built once per session instead of twice.
  `glow::Context::from_loader_function` resolves 709 symbols through
  `eglGetProcAddress`, which costs 41-50 ms on an arm64 device, and it was
  being paid twice: once by `CanvasManager`, with the host thread waiting on
  it, and again by the render thread immediately afterwards, delaying the
  first frame by the same amount. Both were built on the render thread from
  EGL contexts in one share group, so the manager's table now serves both.
  Game-ready drops a further 40-53 ms.
- The log level a host configures now takes effect. `tracing` caches what it
  thinks of a callsite the first time it sees one, and the dynamic level filter
  did not opt out of that: a callsite first reached while the process default
  was `Warn` was cached as disabled and stayed dead, so a host that asked for
  `Info` got silence -- including for the startup timings, which are emitted
  while the host is being built and so could never be seen.
- The host event loop no longer logs a warning when it parks with nothing
  pending. It does that three times on every single launch, in the window
  before the game registers its first `requestAnimationFrame`; a warning on
  the guaranteed path is how a log teaches its reader to skip warnings.
- On arm64, `libmigo.so` is about 1.85 MiB smaller. `.rela.dyn` was 2.17 MB,
  99.8% of it single-word `R_AARCH64_RELATIVE` entries at 24 bytes each;
  `--pack-dyn-relocs=android` group-encodes them, and bionic has decoded that
  format since API 23. `.text` is byte-for-byte identical. A first size-budget
  gate holds the engine to a ceiling from here on, with the unchanged previous
  release as its red case.
- The thirteen rustflags `engine/.cargo/config.toml` declares for each Android
  target now reach the AAR link. Cargo replaces rather than merges
  `[target.<triple>].rustflags` when `RUSTFLAGS` is set in the environment, and
  the AAR build sets it — so on the one build that ships, the config's flags
  were silently discarded.
- `createVideoDecoder`'s stub answers `"createVideoDecoder:fail not supported"`
  instead of accepting `start()` and then never delivering a frame or an event.
  Content that polled `getFrameData()` or waited on a listener waited forever,
  with nothing in the log.
- The BSL `Change Date` is re-stamped to publication + 4 years on every release
  and gated against decaying, exceeding the four-year ceiling BSL 1.1 itself
  imposes, or drifting from `LEGAL.md` and the READMEs.
- The C ABI header no longer implies that a production host can both clear
  `MIGO_ENGINE_FLAG_ALLOW_UNSIGNED_CONTENT` and load content: doing so without
  also supplying a code-signing public key fails with `CodeSignatureInvalid`.
  The two paths that actually work — signing enabled with a key, or the flag
  set — are now stated.

### Removed
- The `wx` namespace. The engine used to install both `migo` and `wx` at
  bootstrap; it now installs only `migo`. Content written for a mini-game
  platform gets that platform's global from an external compatibility adapter
  (`migo-wx-adapter`), which is where that surface belongs. `adapter/` left the
  repository with it — Migo is a pure engine.

### Fixed
- `test-android-host-api-contract.sh` froze nothing an embedder reaches by
  subclassing. It read the surface with `javap -public`, so
  `MigoGameActivity`'s `onCreateGameListener`, `onLaunchFailed`,
  `onSessionCreated` and `getGameSession` -- the documented, "zero-boilerplate"
  integration path -- were outside the freeze along with every other protected
  member. R8 once marked exactly those methods `final` in the release AAR and
  external subclasses stopped compiling; this gate could not have seen it. It
  now reads `-protected`, and the baseline grew by the 13 members that were
  never pinned.
- An unparseable `ctx.font` assignment is a no-op again, on both sides of the
  engine. WHATWG says an invalid value leaves the previous font in effect, and
  the render thread already did that -- it rejected the shorthand and kept its
  state. The JS thread did not: it stored the string and `measureText` answered
  from a best-effort parse of it. So a typo'd font string measured at one size
  and painted at another, silently. `ctx.font = "definitely not a font"` after
  `64px "..."` measured 36 px of text the engine painted 221 px of. The check
  now happens once, in `op_set_font`, ahead of both -- so an invalid value never
  reaches either side and `ctx.font` never reports one. The strict parser moved
  to `shared` to make that possible, which is also what finally makes the
  "one parser, one source of truth" comment in the 2D context true.
- `login`, `getUserInfo` and `getPhoneNumber` return a Promise, like every
  other API on that surface. They resolved immediately on `undefined`, so
  `await migo.login()` did not wait and content ran on as though sign-in had
  finished.
- Concurrent first `setStorage` calls no longer lose the SQLite WAL pragma
  race. Ten writes issued together intermittently failed one; the same ten
  issued sequentially never did.
- A throwing content callback no longer swallows the callbacks queued after it.
  A `success` handler that threw stopped `complete` from running, so content
  that hid a loading spinner there left it on screen.
- The image-cache cumulative trim-bytes counter saturates instead of wrapping.
- Three engine frame-loop internals are non-enumerable on `globalThis`, like
  their siblings — a `for...in` over the global no longer walks them.
- `libc++_shared.so` is no longer packed next to `libmigo.so`. Nothing loaded
  it: `librusty_v8.a` carries Chromium's libc++ statically and `libmigo.so`
  declares no `DT_NEEDED` for the shared one. Two megabytes per ABI, gone.
- `minimp3`'s safe wrapper is dropped, taking `slice-ring-buffer`
  (RUSTSEC-2025-0044, four double-frees reachable through safe APIs, no fixed
  version) out of the tree. Only the raw `mp3dec_*` C entry points were ever
  called. `cargo audit` is clean.
- Several WebGL state calls (`bindBuffer`, `blendColor`, `polygonOffset`,
  `bindTexture`) now go through the same redundant-call dedup cache as their
  peers.

---

## v0.9.3 (2026-08-15)

The last platform gap closes: Android, Linux, Windows, and OpenHarmony each
now build and publish both their x86_64 and arm64 release assets entirely in
CI. No platform's release depends on a local or native-machine build step
any more — release-windows-arm64 and Linux's arm64 addition are this cycle's
own new jobs, and this is the first tag either has ever actually run under.

### Added
- `release-linux` now builds and publishes `aarch64-unknown-linux-gnu`
  alongside `x86_64`, cross-compiled from the same x86_64 runner
- V8 built for `aarch64-pc-windows-msvc`
- ANGLE built from source for Windows arm64 -- the first ANGLE-from-source
  pipeline for any Windows arch this project has shipped
- `release-windows-arm64`, a native `windows-11-arm` CI job producing a real
  Windows arm64 SDK package, wired into `publish`
- Runtime generation fencing, callback correlation, and verification lanes
- Per-session isolate support
- BLE notification path and audio realtime gates
- OpenHarmony API floor declaration gate
- Session delivery and verification lanes (A12)

### Fixed
- `fetch-v8-archives.sh`'s Windows-target file naming matched the literal
  `x86_64-pc-windows-msvc` only, so `aarch64-pc-windows-msvc` fell through to
  a Unix-style name (`librusty_v8-aarch64-pc-windows-msvc.a`) that was never
  published, 404-ing every fetch
- The x86_64 V8 component manifest's recorded hash and toolchain provenance
  described a rebuild that was never actually uploaded to the archive
  release, so `release-windows` failed sha256 verification against the
  archive that was actually still there
- `engine/.cargo/config.toml` pinned `CC`/`CXX` to `clang-cl` for
  `x86_64-pc-windows-msvc` only; `aarch64-pc-windows-msvc` had no
  corresponding pin, so `cc-rs` fell back to probing versioned compiler names
  (`clang-18`, ...) that don't exist on a runner with only choco's LLVM on
  PATH, failing `ring`'s build script
- A canvas the content never sized now follows a surface that was destroyed and
  recreated at a different size, instead of keeping the size derived from the
  previous one. Rotating while the app is in the background takes that path on
  every Android device, and content came back to a `canvas.width`/`height`
  describing the window it was suspended on, stretched across the new one by the
  presentation blit while `migo.getSystemInfoSync()` reported the real extent.
- A canvas the content *did* size with `canvas.width` is no longer moved when the
  surface resizes. It had been rescaled in proportion to the surface, so a game
  that picked a fixed resolution kept drawing in coordinates its own backing
  store no longer had — into a corner of it.

### Changed
- `MigoSurfaceDescriptor.generation` now documents the rule the C ABI already
  enforced: every attach must carry a generation strictly greater than any the
  session has accepted, and a metrics update carries the live attachment's own.
  A host that stamps a constant is refused with `MIGO_ERROR_STALE_SURFACE` from
  its second attach onwards, which any platform that destroys and recreates its
  window — Android on every trip through the background — reaches on the first
  resume.

---

## v0.9.2 (2026-08-13)

One release, one asset naming scheme, one publisher. Every platform is now
built and staged the same way and named `migo-<version>[-capi]-<platform>[-<arch>].<ext>`,
replacing the three schemes v0.9.1 shipped side by side
(`migo-full-release-arm64-v8a.aar`, `migo-sdk-android-arm64-v8a.tar.gz`,
`migo-linux-x86_64.tar.gz`). This is also the first release where the Windows
and OpenHarmony packages are produced by the same reproducible path as
Android and Linux, rather than hand-tarred.

### Added
- `migo-<version>-android.aar` — the single Java/Kotlin AAR (universal, both
  ABIs); the slim and arm64-only AAR variants are no longer published (a
  consumer's own `abiFilters`/App Bundle already owns that choice)
- `migo-<version>-capi-<platform>-<arch>.tar.gz` for `android`, `linux`,
  `windows`, and `ohos`, each with a package manifest and reproducible
  packaging (`scripts/package-sdk.sh`)
- `migo-<version>-sbom.cdx.json`, `SHA256SUMS.txt`, `version.json`
- V8 startup snapshots now embed on Linux (host + worker, full profile), not
  only Android — `runtime-v8/build.rs` dispatches embedding by `(os, arch)`
  instead of an Android-only check. Snapshot filenames carry an OS segment
  (`SNAPSHOT-<profile>-<os>-<arch>.bin`) so android-x86_64 and linux-x86_64,
  previously colliding, are distinct files
- `scripts/test-capi-snapshot-embedding-contract.sh`: proves a shipped C-ABI
  static/shared library actually contains the snapshot bytes its package
  manifest claims, the same property the AAR contract already proved for
  `libmigo.so`
- Windows: `x86_64-pc-windows-msvc` V8 built and its component manifest
  sealed for the first time; the archive (`rusty_v8.lib` + `rusty_v8.dll` +
  import library) is published on the `v8-archives-e6a88b3` release and
  fetchable via `scripts/fetch-v8-archives.sh x86_64-pc-windows-msvc`
- Windows: ANGLE's runtime (`libEGL.dll`, `libGLESv2.dll`,
  `d3dcompiler_47.dll`) is pinned to a verified download
  (`contracts/artifact-manifest/windows-angle.lock.json` +
  `scripts/fetch-windows-angle.sh`) instead of an ad hoc local directory —
  ANGLE publishes no official prebuilt Windows binaries, so these are
  self-hosted on the same release tag the V8 archives use
- OpenHarmony: `librusty_v8-{aarch64,x86_64}-linux-ohos.a` published on
  `v8-archives-e6a88b3`, and OHOS builds in CI (`release-ohos`)
- `scripts/verify-release-assets.sh`: enforces that every published asset is
  covered either by `SHA256SUMS.txt` or its own `.attestation.json`, checked
  against the live GitHub release rather than build intent
- `scripts/test-release-asset-naming-contract.sh` and
  `test-release-asset-ordering-contract.sh` guard the naming scheme and the
  publish job's asset list against drift

### Changed
- `release.yml` restructured per platform: `release-android`, `release-linux`,
  and `release-ohos` build and stage in parallel; a single `publish` job merges
  every platform's staged output, generates one `SHA256SUMS.txt` covering the
  whole release (previously only the Android job's output), and performs one
  upload. Windows is not in CI yet (`build-windows-sdk.sh` needs WSL/`cmd.exe`
  interop a `windows-latest` runner does not have) and is built and uploaded
  by hand until a Windows-native job exists
- The Android V8 archive directories and release asset names moved from bare
  architecture words to full target triples (`aarch64` →
  `aarch64-linux-android`, `x86_64` → `x86_64-linux-android`), matching the
  vocabulary the Linux and OpenHarmony directories already used
- `scripts/publish-release.sh`, whose `required_files` still named v0.9.0-era
  assets and had fully diverged from `release.yml`, is removed

### Fixed
- `SHA256SUMS.txt` no longer silently covers only the Android job's output
  while implying whole-release coverage — see the `release.yml` restructuring
  above
- A release AAR could previously claim an embedded snapshot in its slice
  manifest without the shipped `.so` actually containing it (`build.rs` fails
  safe and only warns on a stale/invalid snapshot); `scripts/test-android-
  snapshot-embedding-contract.sh` and its new C-ABI sibling now read the
  shipped bytes to prove this rather than trusting the manifest

---

## Engine — v0.9.0 (2026-07-28)

First public engine release. Ships the Rust multi-crate engine with a C ABI
(`libmigo_capi`) on four platforms: Android, Linux, Windows, and OpenHarmony.

### Added
- WebAudio-style runtime with `AudioContext`, `AudioBuffer`, and
  `InnerAudioContext` APIs compatible with mini-game style
- Audio decoders for MP3, OGG, and WAV formats; streaming and caching pipeline
- Canvas 2D and WebGL rendering APIs
- File I/O (sync and async), network fetch, and touch input
- C ABI (`migo-capi`) with a documented, controlled export surface (`migo_*`) --
  still a candidate today, not yet frozen; see `include/migo/README.md`
- Android JNI bindings and AAR packaging (`migo-full-release.aar`,
  `migo-slim-release.aar`); Android demo project in
  [migo-examples](https://github.com/minigame-labs/migo-examples)
- Android C-API SDK tarballs (`migo-sdk-android-arm64-v8a.tar.gz`,
  `migo-sdk-android-x86_64.tar.gz`)
- Linux x86_64 C-API SDK (`migo-sdk-linux-x86_64.tar.gz`); see `linux-sdk-0.1.0`
  below
- Windows x86_64 C-API SDK (`migo-sdk-windows-x86_64.tar.gz`); see
  `windows-sdk-0.1.1` below
- OpenHarmony (aarch64 and x86_64) builds and contract gates; no published
  release yet (see known gaps in `dist/migo-ohos-x86_64/share/migo/ohos-x86_64-manifest.json`)
- Prebuilt V8 archives for Android and Linux distributed via release assets
  (release `v8-archives-e6a88b3`, 2026-07-25)
- `scripts/fetch-v8-archives.sh` fetches and verifies prebuilt V8 archives
  against committed component manifests
- `release/VERSION` as the single version source; `scripts/test-release-version-contract.sh`
  enforces that all build consumers derive from it

### Changed
- Renamed project from `minigame_host` to `migo`
- Renamed SO library from `libminigame_host.so` to `libmigo.so`
- V8 archives moved from Git LFS to release assets to avoid LFS quota exhaustion

---

## Linux SDK — linux-sdk-0.1.0 (2026-07-28)

Packaged alongside `v0.9.0`. The Linux SDK carries its own version series because
it is a separately consumable artifact with its own ABI and loader-floor contract,
distinct from the engine's feature version.

### Added
- `libmigo.so` (versioned, soname `libmigo.so.1`) and `libmigo.a` for
  `x86_64-unknown-linux-gnu`
- glibc 2.31 / GLIBCXX 3.4.28 loader floor, enforced by building against the
  Debian bullseye amd64 sysroot (Chromium's pinned sysroot)
- Export surface controlled by a version script; only the documented `migo_*`
  entry points are exported
- CMake `find_package(migo)` support, `pkg-config` `.pc`, and public headers
- Package manifest (`linux-x86_64-manifest.json`) with sha256 hashes and
  provenance; verified by `scripts/test-linux-sdk-contract.sh`
- Qt 6 host kit (`platforms/linux/host-kit/`) with X11 surface view and managed
  session; gated by `scripts/test-linux-qt-host-kit.sh`

---

## Windows SDK — windows-sdk-0.1.1 (2026-07-29)

The Windows SDK carries its own version series for the same reason as the Linux
SDK. `windows-sdk-0.1.1` supersedes `windows-sdk-0.1.0` (never publicly tagged):
`0.1.0` shipped a DLL that loaded and exported all entry points but could attach
no Win32 surface; `0.1.1` adds the Win32 HWND platform layer.

### Added
- `migo.dll` (x86_64, MSVC) with a `.def`-controlled export surface restricted to
  documented `migo_*` entry points
- `migo.lib` import library, `rusty_v8.dll`, and ANGLE runtime DLLs
  (`libEGL.dll`, `libGLESv2.dll`, `d3dcompiler_47.dll`) shipped alongside
- CMake `find_package(migo)` support targeting the MSVC toolchain
- Win32 HWND surface platform layer; the DLL loads and reports
  `MIGO_PLATFORM_WIN32_HWND` as an attachable kind
- Contract gate (`scripts/test-windows-sdk-contract.sh`) that loads the DLL and
  exercises `migo_query_capabilities` to verify surface support is present

---

## Versioning

This project uses [Semantic Versioning](https://semver.org/):

- **MAJOR**: Incompatible API changes
- **MINOR**: New functionality (backward compatible)
- **PATCH**: Bug fixes (backward compatible)

Per-platform SDKs (Linux, Windows) carry their own version series
(`linux-sdk-X.Y.Z`, `windows-sdk-X.Y.Z`) because each is a separately consumable
artifact with its own ABI contract. The engine version (`v0.9.0`) and the
per-platform SDK versions can move independently.

### Pre-1.0 Policy

While the version is below 1.0.0:
- MINOR version bumps may include breaking changes
- PATCH version bumps are backward compatible

[Unreleased]: https://github.com/minigame-labs/migo/compare/v0.9.3...HEAD
[v0.9.3]: https://github.com/minigame-labs/migo/releases/tag/v0.9.3
[v0.9.2]: https://github.com/minigame-labs/migo/releases/tag/v0.9.2
[v0.9.0]: https://github.com/minigame-labs/migo/releases/tag/v0.9.0
[linux-sdk-0.1.0]: https://github.com/minigame-labs/migo/releases/tag/linux-sdk-0.1.0
[windows-sdk-0.1.1]: https://github.com/minigame-labs/migo/releases/tag/windows-sdk-0.1.1
