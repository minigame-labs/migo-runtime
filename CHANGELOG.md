# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed
- WebGL: a present no longer takes the content's framebuffer binding with it. The
  engine presents on the display clock, which can land between two of the content's
  GL batches, and the swap-time blit rebinds the read and draw framebuffers; it then
  re-pointed `FRAMEBUFFER` at the default one. Content that had a render target bound
  and keeps its own cache of what is bound (three.js, like most engines) did not
  rebind, and its next passes drew onto the canvas: `PMREMGenerator` returned a black
  environment map about one run in twelve, and a multiple-render-target scene
  intermittently read black. The present now puts back exactly the read and draw
  bindings it found. Found by running migo-conformance's engine bundles repeatedly;
  `engine-three-advanced` gains a scene that runs the generator 40 times with the
  frame clock ticking (24-28 black maps before, none after).
- Canvas2D: a `getImageData` whose pixels were read while the renderer presented could
  come back all zeros -- about one read in 400 on an iPhone 12 (Performance+), where
  the capture and the read of `.data` are a socket round trip apart. The renderer
  dropped its snapshot pool at every present; it now drops only the snapshots of
  content frames that have ended (a presenting packet), and keeps the frame being
  built. Found by running migo-conformance's suites on a device.
- Canvas2D: an `ImageData` from `getImageData` read after its frame has ended no longer
  reads as zeros. The facade captures a GPU snapshot and reads it back only on `.data`,
  and the renderer keeps a snapshot for one frame: a stored `ImageData` used for
  hit-testing was all transparent a frame later, on every platform (a 100 ms delay was
  enough). The frame-end pass now reads back every `ImageData` nobody has read and no
  `texImage2D`/`texSubImage2D` has consumed -- the readback a browser does at the call,
  taken when it can no longer be avoided; the text-label pattern the snapshots exist
  for (`texImage2D(imageData)` in the same frame) still costs no readback. The facade
  also counts snapshot bytes (32 MiB, half the render pool's 64 MiB) and reads eagerly
  past them: a capture the pool refuses reached nobody, and sixty 1 MiB captures read
  later would have been all zeros from the 64th megabyte on.
- iOS (Performance+): the on-screen canvas reports the size the renderer gives it
  (the surface in CSS pixels: 390 x 844 on an iPhone 12), not the physical surface
  (1170 x 2532). Content that sized its drawing from `canvas.width` drew at three
  times the canvas it was drawing on, and a `getImageData` past the real canvas
  came back empty. A size the content sets is still its own.
- iOS: an image that finished decoding before the on-screen 2D canvas had been
  drawn on or read left that canvas unwritable -- every later `fillRect`,
  `drawImage` and `putImageData` read back as transparent black -- until a WebGL
  context existed (measured on an iPhone 12, ANGLE Metal). The render thread now
  flushes its own queue after taking an upload from the upload thread. Found by
  running migo-conformance's `image-decode` suite on a device.
- Canvas2D: `drawImage` accepts a canvas as its source, in all three forms and
  including the canvas itself and the on-screen canvas. It silently drew nothing:
  the facade only knew images the host had decoded. A new 2D record
  (`DRAW_CANVAS`, 570) carries it; the renderer copies the source into a texture
  the destination can see, keeps that one copy for every later draw of the same
  unchanged canvas (200 draws of a 256x256 canvas took 264 ms with a copy per
  draw and 51 ms with the cache), and drops it when the source is painted on,
  resized or destroyed. Not yet: a WebGL canvas as a source, and `createPattern`
  with a canvas.
- Canvas2D: `fill("evenodd")` and `clip("evenodd")` honour the rule. The argument
  was dropped and every fill and clip was nonzero, so the holes of an even-odd
  shape (icons, rings, cut-outs) came out solid. Two new 2D records
  (`FILL_EVEN_ODD` 568, `CLIP_EVEN_ODD` 569) carry it
  (`contracts/frame-wire/wire-v1.md`, amendment of 2026-10-01).
- Canvas2D attributes now keep their previous value when assigned what the
  specification rejects, instead of storing it: `lineWidth`/`miterLimit` that is
  zero, negative or not finite, `globalAlpha` outside 0..1 or NaN (it was
  clamped), an unknown `lineCap`/`lineJoin`/`textAlign`/`textBaseline`, a negative
  `shadowBlur`, a non-finite shadow offset or `lineDashOffset`, a `setLineDash`
  list with a negative or non-finite entry. `lineWidth = NaN` used to read NaN and
  draw nothing. `getLineDash()` reports an odd list repeated to an even one
  (`[5]` is `[5, 5]`) and `shadowColor` reads `rgba(0, 0, 0, 0)` by default.
- Canvas2D methods that take numbers return without doing anything when an
  argument is NaN or infinite (`translate`, `scale`, `rotate`, `transform`,
  `setTransform`, the path methods, `fillText`/`strokeText`), as in a browser: a
  single `translate(NaN, 0)` used to poison the matrix and blank every later
  draw. `arc`, `arcTo` and `ellipse` with a negative radius throw
  `IndexSizeError`.
- `canvas.width` / `canvas.height` convert what they are given as an
  `unsigned long` does: `canvas.width = 1023.75` reads back 1023 (it read back
  1023.75 while the renderer used 1023), a numeric string works (it threw a
  `TypeError`), a negative number takes the default of 300 (it threw an engine
  error). `getContext` answers `null` for a different kind of context than the
  canvas already has, and `createImageData(imageData)` is accepted.
- `getImageData(0, 0, 0, 1)` threw a `ReferenceError` (`DOMException is not
  defined`) instead of `IndexSizeError`: the 2D context now carries its own
  `DOMException` where the host provides none.
- Canvas2D: `imageSmoothingEnabled` now does what it says. The property was never
  sent to the renderer -- assigning it left a plain value on the JavaScript object
  -- so every scaled `drawImage` was sampled bilinearly and pixel art came out
  blurred on every platform. `imageSmoothingEnabled = false` samples the nearest
  texel; it is saved and restored with the rest of the drawing state, survives a
  readback, and is reset by a canvas resize. `imageSmoothingQuality` is accepted
  and validated; all three levels draw the same. Found by the first run of
  migo-conformance on macOS, where a 2x display put the old assertion's sample a
  device pixel off a texel centre. A new 2D record (`SET_IMAGE_SMOOTHING`, 567) carries it
  (`contracts/frame-wire/wire-v1.md`, amendment of 2026-10-01).
- `putImageData` writes pixels. It was an empty function (`// Not implemented`): p5.js's `updatePixels`, EaselJS's filters
  and every game that edits pixels on the CPU changed nothing, found by running both libraries on the real host. The
  specification's algorithm (the dirty-rectangle form, negative extents, WebIDL `long` arguments) is in the 2D facade, and
  the rectangle -- cut to the ImageData's dirty part and the canvas's bounds, so nothing outside is sent -- goes to the
  renderer as one command that Skia's `writePixels` executes: it ignores the transform, the clip, `globalAlpha`, the
  composite operation and the shadow, as the specification says, and converts from `ImageData`'s unpremultiplied RGBA.
  In process it is `op_put_image_data` (a stream op); on the Performance+ lane it is the new record
  `OP2D_PUT_IMAGE_DATA` (571: `H x y width height byte_length | rgba`), written as bands of at most 1 MiB so a record stays
  inside a packet, decoded with the byte count checked against `width * height * 4`. The damage of a put is its exact device
  rectangle, whatever the drawing state. Tests: the renderer under a hostile state (transform, clip, alpha, composite),
  edge clipping, a short buffer (red before, green after), the decoder (order, pixels, wrong byte counts), the
  producer's banding (tiles the rectangle exactly), and the conformance `canvas2d-spec` assertions. Runtime-v8 JavaScript
  changed: joins the snapshot backlog.
- `onended` of `AudioBufferSourceNode`, `OscillatorNode` and `ConstantSourceNode` fires. The nodes accepted the handler and
  told the console that "onended callbacks are not dispatched by the native graph", so Phaser's `complete`, three.js's
  `onEnded` and every sound library that chains one sound after another never heard a sound end. The graph knew all along
  (`prune` already collects the finished source): setting a handler now asks the audio thread to watch that node
  (`op_audio_watch_source_ended`, service op 181, a command in every lane), and when a watched source finishes -- runs out of
  buffer, or `stop()` comes due -- the audio thread sends `HostCommand::AudioSourceEnded`, delivered through the host-bridge
  hook `_internalTriggerAudioSourceEnded` (in process and in the embedded runtime alike). A game that never sets `onended`
  costs the audio thread nothing; a source collected while it still had audio to play reports nothing; an end is reported
  once. Tests: the context (runs to its end, stopped, collected, unwatched), the embedded dispatch, and the runtime with a
  fake audio thread. Runtime-v8 JavaScript changed: joins the snapshot backlog.
- `InnerAudioContext`'s `canplay` listeners run after the duration is known. The context fetched `duration` from the
  audio thread with an async op when the native `canPlay` arrived and fired the listeners without waiting for the answer,
  so every `onCanplay` callback read `duration === 0` (it was right 50 ms later); Howler.js's HTML5 path computes its end
  timer from `node.duration` in that callback, got a zero-length sound and ended it before it started. Events that arrive
  while the state is being fetched now wait behind it, so listeners still see canplay before the play of an autoplaying
  source (a test plays the audio thread: red before, green after). Runtime-v8 JavaScript changed: joins the snapshot backlog.
- Uniform-block introspection: `getActiveUniformBlockName`, `getActiveUniformBlockParameter`,
  `getUniformIndices` and `getActiveUniforms` exist (PlayCanvas 2.x reads every block's name while it links
  a shader, and stopped there with a TypeError). The driver answers, through four more numbers of the generic state
  query (`gl_state::ACTIVE_UNIFORM_BLOCK_NAME` .. `ACTIVE_UNIFORMS_PARAMETER`; no new wire kind, op or host
  dispatch arm), in an envelope that carries either the value or the specification's error, so an unlinked program,
  a block or uniform index past the count and a pname WebGL leaves out (`UNIFORM_NAME_LENGTH`,
  `UNIFORM_BLOCK_NAME_LENGTH`) raise INVALID_OPERATION / INVALID_VALUE / INVALID_ENUM on the context instead of
  reaching the driver. A list longer than the wire's name limit goes in pieces. ANGLE answers a `getActiveUniforms`
  list longer than the program's active uniform count with zeros; the driver is asked about each uniform once and
  every requested index is answered from that. 43 new conformance assertions pin std140 sizes, offsets and strides,
  the long lists, and each error; with PlayCanvas 2.22.6, Babylon.js 9.29.0, Pixi, Phaser and three.js now all run
  their scenes.
- `getFramebufferAttachmentParameter` exists (Phaser 3.90 asks it while it boots: a TypeError that
  stopped the game before its first frame). The facade now records what is attached to each framebuffer, so
  the object (the wrapper the content holds, not a GL name), its type and its level are answered from
  that, and the default framebuffer follows the specification (`BACK`/`DEPTH`/`STENCIL`, present as the
  context attributes say, anything else INVALID_ENUM). What it cannot know -- an attached texture's component
  sizes, type and colour encoding -- is the driver's, through the second number of the generic state query
  (`gl_state::FRAMEBUFFER_ATTACHMENT_PARAMETER`; no new wire kind, op or contract line). With it Phaser 3.90.0,
  Pixi 7.4.3 and three.js 0.186.1 all run their scenes on real pixels (conformance `engines/`).
- Phaser 3 renders: dynamic text, `useProgram(null)`, integer uniforms. Found running Phaser on the runtime
  (graphics, a canvas texture, `Text`; 5 of 6 checks passed, and then 6 of 6):
  - **A canvas drawn and uploaded in the same frame became an empty texture.** `texImage2D(canvas)` of a
    canvas with pending draws goes through a snapshot record whose `format`/`type` are zeros (the facade has
    none to forward for the six-argument form); the GPU budget refuses an unsized internal format that does
    not equal its format, so the upload was rejected -- logged once as "WebGL GPU storage rejected" -- and the
    texture was never allocated. A texture without storage samples (0,0,0,1): Phaser's `Text` was an opaque black
    box. Every text sprite built this way (Phaser, Cocos Labels) was affected; a canvas that had been read
    back first went through the other path and worked, which is why no test saw it. The snapshot is RGBA8
    pixels, so the budget now accounts it as such (`canvas_source_upload_format`).
  - **`useProgram(null)` failed** with "program not found: 0" and left the previous program bound; it unbinds.
  - **`uniform2i`, `uniform3i`, `uniform4i` did not exist** (WebGL 1; Phaser sets its integer vector uniforms
    with them): now defined, over the array forms' records.
  - `MIGO_GL_TRACE_FAILURES=1` makes the render thread log the command that failed beside its error. The error
    named what went wrong, not which of the thousand calls of a frame asked for it, and finding that took bisecting
    the content; it is how the first two items were found.
- Pixi v7 renders: `getInternalformatParameter` exists. Pixi asks `gl.getInternalformatParameter(RENDERBUFFER,
  ..., SAMPLES)` while it creates its renderer and three.js when it makes a multisampled target, and the
  facade did not define it (a TypeError at construction). It is the driver's answer, descending as the
  WebGL 2 specification promises, empty for a format that cannot be multisampled (ES 3.0 cannot multisample
  an integer renderbuffer, which ANGLE's ES 3.1 reports anyway), `null` with INVALID_ENUM for a target other
  than RENDERBUFFER or a pname other than SAMPLES. With it a Pixi scene -- graphics, a canvas sprite, Text,
  alpha blending, a render texture, the ticker -- draws the right pixels (10 checks on a real device; they
  crashed the process three times before this: a zero-sized canvas, `Intl`, and this).
  Built on a new synchronous query kind, `STATE` (`gl_query::STATE`, 16): JSON text, one kind for every
  query that reads a value or a list back and has no reply shape of its own, selected by a number in
  `frame_wire::sync::gl_state`. The 40-odd methods of the specification that are still missing
  (`getUniform`, `getVertexAttrib`, `getIndexedParameter`, the uniform-block family, ...) need no new
  wire kind, producer op, contract entry or host dispatch each: a number and a handler arm.
- `Intl`, `toLocaleString`, `localeCompare` and `normalize` work, and `new Intl.DateTimeFormat()`
  no longer aborts the process. V8 does not carry its own locale data: the embedder hands it
  ICU's data before the first isolate, and `deno_core` does that only with its
  `include_icu_data` feature, which this runtime built without. So `new
  Intl.NumberFormat('en-US')` threw `TypeError: Internal error. Icu error.`, and
  `new Intl.DateTimeFormat()` or `new Intl.Segmenter()` crashed the process (a V8 CHECK,
  not an exception; a segfault in the unit tests). Nothing had ever formatted a number
  in a test, and a game that calls `score.toLocaleString()`, sorts names with
  `localeCompare`, or runs Pixi's `Text` (which builds an `Intl.Segmenter`) was one line
  from it. The full data is 10.7 MB, a quarter of the Android library, so the new
  `migo-icu-data` crate reads deno_core's pinned ICU 77 file at build time and links only
  what `policy.rs` keeps: 6.8 MB. Kept, because what is wrong when it is missing is a
  wrong answer: the number/date/plural data of every locale (without a locale's file a
  German player's `1234.5` is `1,234.5`), the time zone rules, the break iterators and
  their dictionaries (`Intl.Segmenter`, Chinese word breaking), Chinese collation
  (pinyin/stroke order for a leaderboard), and the display names of 16 languages. Dropped:
  character converters, transliteration, number spell-out, and the display names and
  collation tailorings of the other locales (they fall back to the root). Installed once,
  before the first runtime of any kind (the main one, the prewarmed one, snapshot
  generation). The data is stored uncompressed so ICU reads it in place and only touched
  pages become resident. The filter is code (a 150-line reader/writer of ICU's package
  format with tests, no tool and no committed blob) and the Android size budgets move by
  its size, with the reason written next to them. Found running Pixi on the runtime;
  migo-conformance's `intl-spec` (27 assertions) crashes the released v0.9.19 and passes
  here.
- Asking for WebGL on a canvas nobody has sized yet no longer takes the process down.
  `document.createElement('canvas')` is zero-sized until something sizes it, and
  Pixi, Phaser and three.js all test for WebGL on exactly such a canvas before they
  make their own. The offscreen canvas's EGL pbuffer was created at the canvas's size,
  zero by zero; on a Mac ANGLE turned that into a zero-width Metal texture descriptor,
  Metal's validation asserted, and the whole process aborted -- no JavaScript
  exception, no log line -- the first time a game merely looked for WebGL (on drivers
  that refuse a zero pbuffer, `createCanvas()` failed instead). The pbuffer is only
  the surface `eglMakeCurrent` needs and is now never smaller than one pixel
  (`pbuffer_extent`, at both places one is made), and `drawingBufferWidth/Height`
  say what the WebGL specification says for a zero-sized canvas: "A 0x0 canvas will
  yield a 1x1 drawingBufferWidth/Height". Found running Pixi on the runtime;
  migo-conformance's `webgl-zero-size` aborts the released v0.9.19 and passes here.
- three.js can create a WebGLRenderer, and a canvas texture is the right way up.
  Found by running three.js (r1xx) on the runtime for the first time, which no test
  had done: `new THREE.WebGLRenderer()` threw `TypeError: expected i32` out of
  `getParameter(gl.MAX_SAMPLES)`. The WebGL 2 facade had never declared 160 of the
  specification's constants (`MAX_SAMPLES`, `TEXTURE_3D`, `COLOR_ATTACHMENT1..15`,
  `DRAW_BUFFER0..15`, `UNIFORM_BLOCK_*`, `READ_BUFFER`, `RED`, `RG`, ...), so `gl.X`
  was `undefined` and went to a native op as an argument. All 559 constants of
  Khronos's WebGL 1 and 2 IDL are now declared with the specification's values.
  With that, a three.js scene (basic and PBR materials, a sphere, a data texture, an
  animation loop) renders correctly, with one more fix: `texSubImage2D(canvas)` ignored
  `UNPACK_FLIP_Y_WEBGL` and `UNPACK_PREMULTIPLY_ALPHA_WEBGL` (only the whole-image
  `texImage2D` honoured them), and three.js, Pixi and Cocos all allocate with
  `texImage2D`/`texStorage2D` and stream every canvas texture in with `texSubImage2D`,
  so every `CanvasTexture` (every text sprite, every canvas atlas) came out upside down.
  New gate, `scripts/test-webgl-spec-surface-contract.sh`: Khronos's IDL is vendored
  (`contracts/runtime/webgl-idl/`), every constant must be declared with its value,
  and every method must be defined or listed with its reason in
  `contracts/runtime/webgl-spec-surface.json`. The list is the honest size of the gap:
  66 methods are not implemented yet (`uniform2i`..`4i` and the `ui`/non-square-matrix
  uniforms, `vertexAttrib*f`/`I4*`/`IPointer`, `getUniform`, `getVertexAttrib`,
  `clearBuffer*`, `copyTexImage2D`, `getBufferSubData`, ...). Proven red for a wrong
  value, a missing constant, a method neither defined nor listed, a stale entry and a
  reasonless group.
- Three permission checks work again, and a failed call names its API once. Found by
  probing every callback-style `migo.*` API once on a desktop host
  (migo-conformance's new `api-surface-spec`, found at run time from the namespace so
  an API added later is covered; 272 assertions, released v0.9.19 fails 18):
  - `checkUserLocation`, `checkWritePhotosAlbum` and `getWritePhotosAlbum` answered
    `fail: _authSetting is not defined` on every call since the host-owns-the-answer
    refactor (2026-08-01) removed the local map and left these three naming it. The
    checks now read the host's current answer; `getWritePhotosAlbum`, which used to set
    its own entry to `true` (granting itself the permission), asks the host the way
    `authorize` does and answers under its own name.
  - An API a host does not serve (`getClipboardData`, `vibrateShort`, `showToast`,
    ... on a host without that service) failed with its name twice,
    `getClipboardData:fail getClipboardData:fail not supported`: the op answers a whole
    message and the caller put the prefix on it again. One composer
    (`failMessage`, in `02_async.js`) now builds every `<api>:fail <why>`, and the
    open-coded sites in the login, update, payment, camera, audio, websocket and
    download/upload modules use it.
- Input events carry a time on the clock content reads. A host stamps input in its
  own clock -- the system uptime on Apple and Android, which the C ABI does not name --
  and the engine handed that number to content untouched: on macOS a touch arrived
  with `timeStamp` 62,067,560 ms ahead of `performance.now()`. Content compares
  `event.timeStamp` with `performance.now()` and with the requestAnimationFrame
  timestamp (input smoothing, double-tap windows against the frame clock, latency
  probes; every ported web game), so every such comparison was wrong on exactly the
  platforms whose clock disagrees. Touch, key, mouse, wheel and gamepad timestamps now
  go through one converter (`input/00_input_clock.js`): the host's clock is anchored to
  the page's at the first event of a burst of input (a quarter-second gap starts a new
  burst, so the clocks cannot drift apart over a long session or a sleep), inside a
  burst the host's spacing is kept, and the result is never in the future. The header
  documents the contract (`include/migo/input.h`: use one clock per host; do not
  convert). Found by migo-conformance's new `input-touch-spec`, the first bundle that
  sends the game input (a macOS host replays real mouse events through the view's own
  handlers; 20 assertions; released v0.9.19 fails the timestamp one and passes the
  other 19, which is the evidence the coordinate and lifecycle path is right).
  Also: a touch listener that throws is now reported (`console.error`) instead of
  swallowed silently, so a handler that throws on every touch is not a game that
  mysteriously "ignores input"; the listeners after it still hear the event.
- `migo.request` works again for any response that has a body. Since v0.9.10 every
  request that received a non-empty body failed with `request:fail ... read data
  failed: TypeError: expected typed ArrayBufferView` (errno 500): the perf pass that
  made the body read bounded (#225) merged the chunks and handed the result on as an
  ArrayBuffer to the code that decodes a typed array, so `text`, JSON and
  `arraybuffer` responses all failed, and `readAll`'s old Uint8Array contract was
  nowhere asserted. It went unseen because nothing ran the engine's own `request()`
  over a body: the SSRF filter refuses every address a test could listen on, and
  the one fetch test read the body with `core.read` itself. A `data:` URL is answered
  through the same ops, reader and callbacks without a connection, so the new
  `request_through_the_engine` tests (and migo-conformance's `network-spec`) run the
  real code. They also pin what the same pass left wrong:
  - A response with no body (empty, 204, 304, HEAD) arrives as `""` or an empty
    ArrayBuffer, not `null`, so `res.data.length` is not a TypeError.
  - A `success` callback that throws no longer makes the request a failure: the
    delivery sat inside the `try` that reports a failed read, so the app's own bug
    ran its `fail` and `complete` after its `success`. The same shape was in
    `downloadFile` (the download was kept and reported failed) and `uploadFile`. One
    settler now delivers a request's outcome once, each callback isolated, and a
    `fail` that throws no longer escapes into the event loop.
  - `request`, `downloadFile` and `uploadFile` with arguments that cannot make a
    request -- an unknown or non-string `method`, a `null` options object or
    `header` -- fail through `fail`/`complete` with `request:fail ...` instead of
    throwing out of the caller's helper; callbacks that are not functions are ignored.
  - A body that fits one chunk (most JSON) is no longer copied a second time.
- A proxy on the machine no longer breaks every network request. With an HTTP or
  SOCKS proxy configured -- the `HTTP_PROXY`/`HTTPS_PROXY` environment or the
  system setting, which the engine's client honours as the system WebView does --
  every `request`, image and streamed audio fetch failed with `connection to
  127.0.0.1 is not allowed (private/loopback address)` (errno 500) *after the
  server had answered it*: the SSRF check on the address a response came from was
  the proxy's address, 127.0.0.1 for a local proxy and a private one for a
  corporate proxy. That check guarded nothing the client's own resolver and the
  gate before the send do not already guard (a name resolving to a blocked
  address is refused before any connection; an IP-literal URL is refused when the
  request is built, and on every redirect hop), so it is gone, and tests pin both:
  a response a loopback proxy delivers is delivered, and a loopback destination is
  still refused without the listener behind it seeing a connection. Found by
  trying `migo.request` from the macOS conformance host on a machine behind a
  local proxy.
- Starting many reads at once no longer fails most of them. A game does not read
  its assets one at a time: an asset loader starts dozens of `readFile` calls in
  one tick. Each whole-file read was charged the largest read a read can be
  (100 MiB) against the scheduler's pending-byte budget instead of what the file
  is, so sixty reads of a few hundred bytes spent the budget twice over and 37 of
  them failed with `IO pending-byte budget exhausted: requested 104857600 bytes`.
  The same went for the digest (`getFileInfo`) and Brotli read of a package
  entry. A read is now charged the size of the file (one `stat`) or of the
  package entry, bounded by the caller's `length`; one nobody can size keeps the
  cautious estimate, which is what the budget is for. Found by migo-conformance's
  new `io-spec` bundle (released v0.9.19: 23 of 60 succeed), and by a unit test
  that failed one run in twenty for the same reason.
- Workers: `terminate()` is quiet. The worker's event loop ends with V8's
  "execution terminated", which is the answer to the request, and it was reported
  to the content's `onError` (and logged at ERROR) as if the worker had crashed,
  every time. Also: `terminate()` followed at once by `createWorker()` -- restarting
  a worker -- no longer fails or hands back a worker that never starts: the old
  thread needs a moment to leave its isolate, and `createWorker()` now waits for it
  (up to two seconds) instead of racing it. And a worker may post a burst of
  messages: the queue held 64, so a worker that posted a result per item in a loop
  threw `Worker message queue full` at the 65th and the rest never existed (68 of
  200 arrived); it holds 4096 per direction, still inside the same byte budget that
  actually bounds the memory. Found by migo-conformance's new `worker-spec` bundle
  (17 assertions: the released v0.9.19 fails 3).
- `performance.now()`'s origin is clamped to the clock it is read against, which
  matters only to tests that advance a paused clock by hand: the worker timer
  lifecycle tests failed when run on their own, since the shared process origin
  introduced with the frame-timestamp fix could be later than a paused clock's
  "now" and time then stood still at zero until the clock caught up. They passed in
  the full run by accident of ordering.
- The V8 code cache's directory has one name however many sessions start at once.
  Two sessions asking for the cache while its worker was clearing and recreating
  the directory (it does that whenever the stored V8 version differs, which a
  fresh directory always does) could find it missing, fail to resolve it, and key
  the shared registry on the unresolved spelling -- `/var/...` against
  `/private/var/...` -- which gave one directory two caches, two counters and a
  32 MiB budget each. The parent directory is resolved instead, which nothing in
  the module touches. The unit test that guards the shared budget failed one run
  in four on this and was recorded as a flake; the cause was real.
- The first `createCanvas()` of a game no longer fails with
  `[Timeout] get_canvas_info timed out` when the render thread is slow to start.
  The call waited one second for the render thread's answer, and the render
  thread answers only after its own start-up (creating the EGL context, probing
  the GPU, a cold driver): on a slow or loaded machine the call timed out and the
  unhandled rejection ended the game before its first frame. The wait is five
  seconds -- it bounds a hung render thread, not a busy one (a dead one is seen at
  once, and a stuck one is the watchdog's) -- and the presentation-paths CI gate,
  which failed one run in several with "blit-probe never painted" after a cold
  runner spent 3.3 s building the runtime and 5.8 s probing the GPU, stops
  failing for that reason.
- `getSystemInfoSync().platform` and `getDeviceInfo().platform` named the
  platform the engine was built for instead of `"android"` on everything. A host
  that attached no device services (the in-process Apple host, and the desktop
  hosts that implement window info only) was answered with an `"android"` default
  with an empty `system` string, so content branching on the platform -- which
  audio format, which layout, which input model -- decided as if it ran on
  Android on a Mac, a Windows PC or a Linux box. The answer is now `"mac"`,
  `"windows"`, `"linux"`, `"ios"`, `"ohos"` or `"android"` from the build target,
  with `system` leading with the OS name (`"macOS"`), `abi` from the build, and
  nothing invented: `model` stays `"unknown"`, `brand` empty, `benchmarkLevel`
  `-1`. A host that can say more (a model, an OS version) overrides it, as the
  Android host and the iOS Performance+ profile already do. Found by
  migo-conformance's new `system-info-spec` bundle (37 assertions: the released
  v0.9.19 fails the one that checks the platform against the OS string).
- File system: `readFileSync(path, "base64")` (and `readFile`, `unzip`'s readers)
  threw `btoa is not defined`: the encoder called the page's `btoa`, which this
  runtime does not have without an adapter. It uses the native codec the other
  encodings use. `writeSync` / `write` returned `{ bytesWritten }` as a BigInt, so
  `bytesWritten + 1` was a `TypeError` and `JSON.stringify` of the result threw;
  it is a Number now, as `readSync`'s `bytesRead` already was.
- File system: a failed `renameSync` / `copyFileSync` (and the async forms) told
  the content the host paths the virtual ones resolved to -- `rename
  /Users/<name>/Library/Application Support/Migo/files/.../user_data/x -> ...` --
  which is the user's home directory and the game's sandbox location, in the text
  of an error a game may well log or report. It names the paths the content gave.
  The same for a `readdirSync` that meets a non-UTF-8 file name. Found by
  migo-conformance's new `fs-spec` bundle (81 assertions: the released v0.9.19
  fails 8, and two groups of its assertions never ran because the base64 read
  threw first).
- The timestamp handed to a `requestAnimationFrame` callback is on the same
  timeline as `performance.now()`, as in a browser. They were two clocks:
  `performance.now()` counted from the creation of the JavaScript runtime, the
  frame timestamp from the first vsync (or from the render thread's start where
  there is no display clock). A game that had loaded for two seconds read
  `ts = 0` on its first frame against a `performance.now()` of 2000, so a loop
  starting with `dt = ts - performance.now()` had a negative first step and an
  animation timeline anchored to `performance.now()` began two seconds behind.
  Both now count from one process-wide origin (`shared::time_origin`), and the
  vsync clock's timestamps are put on it by anchoring the first frame
  (`RafTimeline`) and keeping the host clock's spacing after that.
  `performance.timeOrigin` exists (it was `undefined`, so
  `performance.timeOrigin + performance.now()` was `NaN`).
- `requestAnimationFrame` after an idle screen or a long frame no longer receives
  a timestamp from before the stall. The render thread signals every vsync while
  animating so a consumer that keeps up never blocks; the signal went through a
  bounded channel that dropped the NEWEST signal when full, so a consumer that
  fell behind was handed the oldest two timestamps the render thread had written
  (on Android's eventfd path the newest, which was always right). The first
  callback after 300 ms of idle got a time 267 ms old, its successor a step of
  that size. Every platform now keeps only the newest unconsumed signal, and a
  signal more than 50 ms older than the request is passed over for the next one.
  Found by migo-conformance's new `timers-spec` bundle (25 assertions: the
  released v0.9.19 fails 3).
- Web Audio: the errors the engine's own audio, `ImageData` and Canvas 2D code
  throw are the ones the specification names. They were written as
  `new DOMException(...)`, and `DOMException` is the host page's to provide: with
  no adapter installed, `createBuffer(1, 10, 1)`, a second `start()` on an
  oscillator, `connect()` to a missing input and `createImageData(0, 1)` threw a
  `ReferenceError` instead of `NotSupportedError`, `InvalidStateError` and
  `IndexSizeError`. One shared factory (`base/06_dom_exception.js`) makes them: it
  uses the adapter's `DOMException` when there is one, so `instanceof` holds in
  content, and its own class (same `name`, `message` and legacy `code`) otherwise,
  and installs nothing on the global object. Also: `createBuffer` / `new
  AudioBuffer` with zero channels or zero length are `NotSupportedError` (they
  were `RangeError`); `getChannelData`, `copyFromChannel` and `copyToChannel` with
  a channel that does not exist are `IndexSizeError` (they were `RangeError`);
  `decodeAudioData` rejects bytes it cannot read with `EncodingError` (it rejected
  with an `AudioError` carrying the engine's text), through the promise and the
  error callback alike.
- Web Audio: `decodeAudioData` makes a buffer at the context's sample rate. It
  resampled to the device's, so a context at 44100 Hz on a 48000 Hz device handed
  content buffers whose `sampleRate` was not its own, and every sample index
  computed from the context's rate was 8.8% off; playback also paid to resample
  them back at run time. Found by migo-conformance's new `audio-spec` bundle (53
  assertions: the released v0.9.19 fails 19 of them).
- WebGL: `pixelStorei(UNPACK_FLIP_Y_WEBGL)` and
  `pixelStorei(UNPACK_PREMULTIPLY_ALPHA_WEBGL)` were recorded and never applied,
  so every upload ignored them: an engine that flips its textures (three.js and
  Babylon do by default) drew them upside down, and one that uploads premultiplied
  (Pixi, Phaser, Egret) got straight alpha and bright fringes. The renderer now
  applies them to `texImage2D` and `texSubImage2D` from bytes (a typed array,
  `ImageData`, a decoded image), reading the rows the way the driver would
  (`UNPACK_ALIGNMENT`, `UNPACK_ROW_LENGTH`, the skips), and to `texImage2D` from a
  canvas. A canvas also keeps WebGL's default of straight alpha: it holds
  premultiplied colour, and was uploaded as it was whatever the flag said, so a
  translucent edge came out too dark. The flags are no longer sent to the driver,
  which has no such parameter and answers `INVALID_ENUM`; `getParameter` reports
  them as booleans (and `UNPACK_COLORSPACE_CONVERSION_WEBGL` as
  `BROWSER_DEFAULT_WEBGL`). Not yet: `texSubImage2D` from a canvas ignores them.
- WebGL: `getParameter(VERSION)` and `getParameter(SHADING_LANGUAGE_VERSION)` begin
  with `WebGL 1.0` / `WebGL GLSL ES 1.00` (`WebGL 2.0` / `WebGL GLSL ES 3.00` on a
  WebGL 2 context) followed by the driver's string in parentheses, which is what
  content and libraries test; they returned the driver's `OpenGL ES 3.0 ...`.
- WebGL: `invalidateFramebuffer(target, 4294967295)`, `drawBuffers(4294967295)` and
  `drawBuffersWEBGL(4294967295)` -- a number where a sequence of enums belongs --
  built a 16 GiB typed array, and the isolate stopped answering until the watchdog
  ended it. They throw `TypeError`, as WebIDL says; `null` and `undefined` remain
  the empty list. Found by a hostile-call test (`robustness-hostile-calls` in
  migo-conformance: 1.8 M random calls with NaN, infinities, 2^32, BigInt, odd
  typed arrays at the Canvas2D, WebGL and WebGL 2 contexts) which otherwise found
  no crash and no other hang.
- WebGL 2: `gl.HALF_FLOAT` was `undefined` (the constant was never declared), so a
  half-float upload -- the `type` of every `RGBA16F` texture -- went to the driver
  with no type.
- WebGL: `isBuffer`, `isFramebuffer`, `isProgram`, `isRenderbuffer`, `isShader`
  and `isTexture` exist (calling one was a `TypeError`), and `getShaderSource`,
  `getBufferParameter`, `getTexParameter` and `getRenderbufferParameter` answer
  from what the content set, without a round trip to the render thread. Not yet:
  `getVertexAttrib`, `getVertexAttribOffset`, `getUniform`.
- WebGL: a context on an offscreen canvas began with a 1x1 viewport and scissor
  box. A GL context takes the size of the surface it is first made current with,
  and an offscreen canvas is created as a 1x1 pbuffer and sized by the content
  afterwards, so every draw was clipped to one pixel until the content called
  `viewport` itself, and `enable(SCISSOR_TEST)` without a `scissor` call clipped
  everything for good. `getParameter(VIEWPORT)` and `getParameter(SCISSOR_BOX)`
  now start at the drawing buffer's size, set when the first GL command reaches
  the canvas.
- Canvas2D compositing: `source-in`, `source-out`, `destination-in`,
  `destination-atop` and `copy` only changed the pixels of the shape being drawn.
  The specification composites against a bitmap that is transparent beyond the
  shape, so for these five everything else the clip allows is cleared (or kept,
  per operator): drawing a circle with `destination-in` is how a picture is
  cropped to a circle, and it cropped nothing. These draws now go through a layer
  composited over the whole clip.
- WebGL: a new offscreen canvas's drawing buffer could begin holding an earlier
  canvas's pixels, the same defect as the Canvas2D one below (measured on macOS:
  77-93 of 360 after the first ~90). A drawing buffer is specified as transparent
  black, depth 1, stencil 0 when created and again when resized; the DrawingBuffer
  is now cleared to that where it is allocated, and a pbuffer canvas at its first
  use after it was created or resized, with the content's own clear values, write
  masks, scissor and rasterizer-discard put back afterwards. Assigning a WebGL
  canvas the size it already has now clears it too.
- Canvas2D: a new canvas could begin holding an earlier canvas's pixels. The
  framebuffer under a fresh surface is whatever the driver returns, and ANGLE's
  Metal backend recycles the storage of destroyed surfaces without clearing it:
  after a few hundred canvases had been created and collected, every new one
  started with the pixels of one that had been freed (measured on macOS: 162 of
  480). Every Canvas2D surface is now cleared when it is created, which also makes
  assigning a canvas the size it already has clear it by construction.
- Canvas2D: `ctx.ellipse(x, y, rx, ry, 0, 0, 2 * Math.PI)` -- a full turn, the
  usual way to draw an ellipse -- drew nothing. Skia's `arc_to` treats a sweep of
  360 degrees as degenerate; `arc` already split a full turn in two and
  `ellipse` did not.
- Canvas2D `getImageData` returned premultiplied colour for translucent pixels:
  half-transparent red read back as `(127, 0, 0, 127)` where `ImageData` holds
  straight alpha, `(255, 0, 0, 128)`. Every translucent edge was darker than it
  is, and a read followed by a write would have darkened it again. The CPU read
  of the snapshot behind `getImageData` now unpremultiplies.
- Canvas2D shadows: `shadowOffsetX/Y` and `shadowBlur` are in device pixels and
  not affected by the current transform, as the specification says; they were
  carried through the matrix, so a context under `scale(2, 2)` -- every
  device-pixel-ratio game -- drew its shadow twice as far and twice as soft as a
  browser. And `globalAlpha` was applied to the shadow twice (once in the
  silhouette, once in the shadow colour): at `globalAlpha = 0.5` the shadow was a
  quarter transparent instead of half.
- Canvas2D text on macOS and iOS: `serif`, `monospace`, `cursive`, `fantasy`,
  `system-ui` (and `-apple-system`, `ui-monospace`, ...) now reach a system face.
  Skia's CoreText font manager answers `Helvetica` and `Menlo` but none of the CSS
  generic keywords, so each used to fall through to the bundled Noto Sans: a
  `monospace` overlay came out proportional, a `serif` heading came out sans, and
  text that named no installed family was drawn in a face no Apple browser picks.
  The keywords now map to the faces Safari and Chrome use (`Helvetica`, `Times`,
  `Menlo`, the system UI font, `Apple Chancery`/`Snell Roundhand`, `Papyrus`);
  Android and Linux keep resolving them natively.
- Canvas2D `measureText` and `getTextLineHeight`, every platform: the JS-thread
  measurement resolved only the first name of the `font` list (plus `sans-serif`)
  while `fillText` resolved the whole list, so `"Microsoft YaHei", serif`
  measured one face and painted another and text laid out from the measurement
  did not fit what was drawn. Both now resolve the same list.
- Canvas2D: assigning `canvas.width` or `canvas.height` the value it already has
  now clears the canvas and resets the context, as the specification says
  (`canvas.width = canvas.width` is the old way to clear a canvas). It cleared
  nothing and reset nothing on the renderer while the JavaScript half reset its
  shadow of the state, so `fillStyle = "#000"` afterwards drew the previous
  colour. A draw still queued when the canvas is resized, to any size, is
  dropped with the old bitmap instead of landing on the new one. The engine's
  own resizes to the size a canvas already has stay a no-op.
- Canvas2D: a `getImageData` on an offscreen canvas could stop the on-screen
  canvas from drawing or reading back anything afterwards. The snapshot code
  kept one temporary framebuffer for every canvas, created in whichever EGL
  context needed it first, and used that name in all the others; a framebuffer
  is not shared between contexts, so in the on-screen canvas's context the name
  was its DrawingBuffer, and each snapshot attached a texture to it and detached
  it again. Each canvas now has its own temporary. Found by the first run of
  migo-conformance on macOS; the same sequence on any platform reaches it.

## v0.9.19 (2026-09-29)

### Fixed
- macOS: on a Mac with two GPUs held on its integrated one (`pmset gpuswitch 0`),
  a game's frame loop ran and its window stayed black; even a Canvas2D read
  back empty. ANGLE took the
  system default GPU -- the discrete one -- whatever drove the display.
  `MigoGameView` now sets its layer's device to the GPU that drives the
  window's display and the engine renders on the layer's device, named to
  ANGLE through `EGL_ANGLE_platform_angle_device_id`. A host attaching its own
  `CAMetalLayer` through the C ABI does the same (`include/migo/platform/macos.h`).
  Measured on a MacBookPro16,1 under each `gpuswitch` setting: the view renders
  on the GPU that drives its display, integrated or discrete.

## v0.9.18 (2026-09-29)

### Removed
- C ABI: the resource-lane declarations in `external_frames.h` --
  `MigoResourceReservationDescriptor`, `MigoResourceOutcome`,
  `MigoResourceState`, `MigoResourceError`. No entry point ever took them and
  nothing implemented the lane; uploads larger than a frame packet are staged
  in the command stream instead (below).

### Fixed
- iOS (Performance+): a WebGL upload larger than one frame packet -- 4 MiB --
  was refused `OUT_OF_MEMORY` on the producer, so it drew nothing: a
  2048-square RGBA texture (16 MiB), a 2048-square ASTC 4x4 one (exactly
  4 MiB), a large vertex buffer. Its bytes now cross ahead of the call as
  staged chunks in the same command stream, over as many packets as they
  need, and the host moves them into the upload whole; up to the 64 MiB one
  upload may carry on every lane. Uploads that fit a packet are sent exactly as
  before. A simulator acceptance test draws with an 8 MiB texture and a 5 MiB
  vertex buffer and reads back both halves of the texture.
- iOS (Performance+): a record that filled the last 8 bytes of a frame packet
  threw instead of being sent -- the writer asked for 8 bytes more than the
  check before it had allowed for.
- Docs: the developer docs describe iOS and macOS as shipped -- an Apple SDK
  reference for `MigoGameView`, six platforms on the home and architecture
  pages -- Linux as the released C ABI SDK it is, and the English architecture
  diagrams render.

## v0.9.17 (2026-09-28)

### Fixed
- Android: the engine's frame-rate request never reached the display.
  `ANativeWindow_setFrameRate` (Android 11+) was not called at all, from
  v0.9.5 on, because the wrapper every window surface passes through did not
  forward the request, so the display picked its refresh rate from its own
  heuristics rather than from the rate the game presents at. On an Android 14
  emulator the game's layer now carries a 60 Hz vote with Default
  compatibility; before the fix it carried none.

## v0.9.16 (2026-09-28)

### Fixed
- iOS (Performance+): a game could start to a black screen, its frame loop
  running and nothing drawn, in about one launch in four on an iPhone XS Max
  under load. WebKit keeps firing a Worker's timers while the Worker is
  blocked in a synchronous request -- the channel the engine's synchronous
  calls (`getImageData`, `getStorageSync`, WebGL queries) take -- so a timer
  could run in the middle of the script that made the call. The web adapter's
  `load` event, queued on a zero-delay timer, then fired while a Phaser bundle
  was still being evaluated, before the game registered for it, and the game
  never started. The producer now holds every timer and socket event that
  arrives during a synchronous call until the script that made it has
  finished, and runs them in arrival order, each as a task of its own. The
  engine's synchronous channel was designed on `Atomics.wait`, which
  dispatches nothing; this restores what it assumed. A device acceptance test
  arms a timer before 300 ms of synchronous calls: without the fix it ran
  inside them on every run. The black screen reproduced on v0.9.14 and v0.9.15
  alike.
- Docs: the Apple guide says the game's orientation has to hold before
  `MigoGameView`'s first layout, not only eventually.

## v0.9.15 (2026-09-28)

### Added
- C ABI (external-frames product): `migo_session_start_frame_endpoint`,
  `migo_session_stop_frame_endpoint` and
  `migo_session_get_frame_transport_statistics` with its
  `MigoFrameTransportStatistics` record. While the endpoint runs it owns the
  downlink, so `migo_session_take_downlink`,
  `migo_session_take_service_message` and `migo_session_set_downlink_waker`
  return `MIGO_ERROR_INVALID_STATE`; a host that brings its own transport keeps
  using them as before.

### Changed
- iOS (Performance+): the engine terminates the producer's socket itself. The
  loopback WebSocket the WebContent producer sends frames on and hears its
  verdicts, frame-clock ticks and answers from used to be the host's --
  Network.framework, every message copied across the C boundary and the
  downlink pumped from a GCD queue -- and on devices that was about a quarter
  of the App process's CPU samples while a game ran (iPhone 15 Pro: 23%, ~3.4%
  of a core), with three queue hops between the frame clock and the socket. Now
  an uplink message is one read and a submit on the engine thread that read it,
  and a downlink message one unpark and one write, with Nagle's algorithm off
  and `SO_NOSIGPIPE` on every connection. Measured on an iPhone XS Max against
  v0.9.14 built the same way, interleaved, 30-second windows, one message each
  way per frame in both: the App process's CPU fell by 4.7 and 5.2 points
  (bunnymark, endless-runner) and was unchanged on canvasmark, and frames
  presented late fell from 13 in 9 runs to 4 in 8. The WebKit helper processes'
  CPU time rose by about as much as the App's fell -- every process ran on the
  efficiency cores, whose clock they share -- so the device's total CPU time is
  about the same; an energy measurement needs a phone that runs Power Profiler.
  `MigoFrameChannel` keeps the content origin's half and its `Statistics`, now
  counted by the engine for both uplinks alike.

### Removed
- Swift: `MigoFrameTransport`, the host-side Network.framework socket the
  engine's frame endpoint replaces.

## v0.9.14 (2026-09-28)

### Fixed
- iOS (Performance+): the web view that hosts the producer is window-sized
  again (still off-screen), undoing v0.9.13's one-point shape. At one point the
  Canvas2D bench game presented more late frames in every one of six
  interleaved 30-second pairs on an iPhone 15 Pro -- 6.3 against 4.0 per 30 s,
  with both builds made the same way and differing only in that frame -- while
  the WebGL game showed no difference. Frame pacing outranks the 11 MiB of
  WebContent memory the smaller view saved, so the window's size stays until
  the cause is understood. v0.9.13's canvas-sized drawable is unaffected.

## v0.9.13 (2026-09-27)

### Changed
- iOS and macOS: when the onscreen canvas is smaller than the window -- as the
  canvas of a game that sizes it in logical pixels is -- the drawable now takes
  the canvas's size and Core Animation scales it to the view, the way a browser
  composites a canvas, instead of the engine upscaling into three window-sized
  drawables every frame. On an iPhone 15 Pro the Performance+ lane's footprint
  fell 41-47 MiB per bench game (bunnymark 184 to 137 MiB, endless-runner 229
  to 188, canvasmark 164 to 119) with CPU unchanged, and frames still present
  at 60 per second with a 16.67 ms p99 interval. It needs fixed-size window
  surfaces, which ANGLE implements only for D3D, so the Apple ANGLE build now
  carries them for Metal (`angle-apple-52f59428-p2`). The drawable's size is
  the engine's: a host must not set `CAMetalLayer.drawableSize`, and
  `MigoGameView` no longer does (`include/migo/platform/ios.h`, `macos.h`).
- iOS (Performance+): the web view that hosts the producer is one point square
  just off the window's edge instead of window-sized. WebKit keeps a backing
  store for the page at the view's size even though the page draws nothing;
  at the window's size that was 11 MiB of WebContent's memory on an iPhone 15
  Pro, with frame rate and CPU unchanged at one point.
- Developer documentation: the Apple page is an integration guide for the
  released SwiftPM package -- the two products and their minimum OS, content
  signing, platform notes, the device capabilities `MigoGameView` answers and
  the ones it refuses, and what has been verified where -- in place of a
  description of the skeleton.

## v0.9.12 (2026-09-27)

### Changed
- iOS and macOS (Performance+): a frame now crosses the WebContent boundary as
  one message each way instead of two. A presenting packet is also the request
  for the next frame-clock tick, and its verdict travels with that tick unless
  the producer cannot wait for it (a refusal, an empty window, or no tick
  owed). On an iPhone XS Max at 60 fps the Performance+ lane's CPU time fell
  7-20% across the bench games, most of it in WebKit's network process, with
  frame rate and memory unchanged. No wire-format change: a host that drains
  after every submit, or a producer that still sends every request, stays
  correct (`contracts/frame-wire/wire-v1.md`, amendment of 2026-09-27).

## v0.9.11 (2026-09-27)

### Fixed
- Every platform: `WEBGL_lose_context.loseContext()` reset the whole GPU share
  group instead of losing the one context it was called on. Pixi calls it on a
  probe context while choosing a renderer, and a reset that landed after the
  game had built its shaders took them away: on an iPhone, Pixi games stayed
  black on their first launch after install. It now loses that context only, as
  the extension specifies; the reset is still reachable for verification as
  `MIGO_debug_gpu_reset`.
- Every platform: `console.error(error)` logged an `Error` as `{}`. It now logs
  its name, message and stack; `undefined`, functions and symbols, which logged
  as nothing, log as their text.
- iOS and macOS (Performance+): a game that draws only with Canvas2D showed a
  black screen -- its batches were never marked for presentation.
- iOS and macOS (Performance+): after a GL context loss every frame was refused
  and the game froze on its last picture, because the producer was never told
  the new resource epoch. The host now sends it (`DOWN_CONTEXT_STATE`, a
  downlink-only addition to frame-wire v1), and content gets
  `webglcontextlost`/`webglcontextrestored` as on every other platform.
- iOS and macOS (Performance+): a web adapter's `XMLHttpRequest` shim broke the
  engine's synchronous calls, and its timers threw `Illegal invocation`; the
  producer now uses the platform objects it captured before content ran.
- iOS and macOS (Performance+): content saw the WebKit Worker's own globals
  (`importScripts`, `self`, `navigator`, ...). Phaser read `importScripts`,
  decided it was in a Web Worker and refused to start. Content now sees the
  same global names the engine publishes on every other platform.

## v0.9.10 (2026-09-26)

### Fixed
- Android C ABI: JPEG images still failed to decode after v0.9.9's fix -- Skia,
  as this engine links it, decodes PNG but not JPEG, and the Java SDK's
  BitmapFactory fallback is out of a C host's reach. The C ABI package now
  carries the Rust image decoders every other platform's C ABI already has; the
  Java SDK's AAR is unchanged. Found by migo-conformance's new image-decode
  bundle, which runs every decode path on both Android embeddings.

## v0.9.9 (2026-09-26)

### Fixed
- Android C ABI: images never decoded, so every WebGL game rendered black while
  its frame loop kept running. Android builds leave the Rust image decoders out
  and rely on the Java SDK to register BitmapFactory at load time, which a C
  host never runs. The C ABI now registers Skia's decoders -- straight into an
  `AHardwareBuffer` where the renderer can import one, into RGBA memory where an
  image must be CPU-backed -- at no binary cost, since Skia is already linked
  for rendering. The Android C host's on-device check now paints its first frame
  from decoded images on both paths, so this cannot pass unnoticed again.

## v0.9.8 (2026-09-26)

### Changed
- The engine now waits up to ten seconds, not two, for the renderer to report
  its GPU capabilities before it runs content. The wait only guards against a
  renderer that has stopped: one that fails reports it immediately, and one
  that comes up is waited for only as long as it takes. At two seconds, a
  slow but healthy first start -- a cold driver shader cache on a low-end
  device, or software rendering on a loaded machine -- was treated as a failure
  and the session refused its content for good.

### Fixed
- Windows: no session could start when a host's files, cache or code-cache
  directory was spelled with `..`, such as the examples' `<files>\..\cache`
  ("initialize sandbox filesystem: failed to pin sandbox roots"). Sandbox roots
  were opened in Win32's `\\?\` namespace, where `..` is a file name rather
  than a step up. A root is now given the meaning Win32 itself gives it before
  it is opened; relative roots are still refused. The Windows arms of the
  sandbox filesystem and the atomic writer now run in CI, which they had never
  done.
- Linux: the offscreen (headless) target needed a window server after all.
  It asked EGL for the default display, which Mesa resolves to X11, so it
  failed to start wherever `DISPLAY` was unset -- every CI runner and server.
  It now uses Mesa's surfaceless platform when the driver offers it, and asks
  only for the pbuffer configuration a headless target renders into.
- iOS and macOS: after `migo_surface_release_query` reported RELEASED, the
  host's `CAMetalLayer` could still be alive for a moment, released on the
  render thread just afterwards. Objects ANGLE autoreleased during the surface
  teardown lived until the render loop's pool drained, which came after
  RELEASED. Teardown now drains them before RELEASED is published, on detach,
  on surface replacement and at shutdown.

## v0.9.7 (2026-09-25)

### Added
- iOS and macOS SDK: `migo-<version>-apple-sdk.zip`, a Swift package with one
  view per platform. `MigoGameView` (`MigoApplePerformancePlus` on iOS,
  `MigoMacV8` on macOS) takes an installed game and owns everything between it
  and pixels -- engine session, `CAMetalLayer`, display clock, input, app
  lifecycle, and on iOS the audio session and WebContent crash recovery.
  `MigoGameInstaller` installs a package atomically and skips a version that is
  already there. Both products ship a privacy manifest that a gate checks
  against the engine's sources in both directions. The zip carries its own
  `.attestation.json`, like every other SDK archive, and a runnable app for
  each platform is `apple-swift/` in migo-examples.
- iOS and macOS: the soft keyboard. `migo.showKeyboard` opens the system
  keyboard on iOS and a text field along the game's bottom edge on macOS; the
  player's text comes back as `onKeyboardInput`/`Confirm`/`Complete`. On the
  iOS lane the three keyboard ops became host commands to the same keyboard
  service the embedded runtime calls, so a host with no keyboard refuses them
  in the same words.
- iOS and macOS: the device. `vibrateShort`/`vibrateLong` drive the Taptic
  Engine on iOS; `setKeepScreenOn` holds the display awake (the idle timer on
  iOS, a power assertion on macOS) and gives it back when the game ends;
  `getGameLogManager().log` entries reach the app as `MigoGameView.Event.gameLog`;
  `getNetworkType`/`onNetworkStatusChange` follow `NWPathMonitor`, and on iOS
  `getBatteryInfo` reports the battery and Low Power Mode. A Mac has nothing to
  vibrate and no battery API a game should need, so those answer "not
  supported" there, as on a device without the hardware.
- C ABI: device capabilities. Three optional, independent callbacks appended to
  `MigoHostCallbacks` -- `on_vibrate`, `on_keep_screen_on`, `on_game_log` -- each
  offered to content exactly when installed, and two report functions,
  `migo_session_set_network_status` and `migo_session_set_battery_status`,
  whose last report the engine answers content's synchronous reads from and
  forwards a network change while content listens. An older host is
  zero-extended to none of them, which is the previous behaviour.
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
- iOS: installing an update over a game that had already run under code
  signing failed on an iPhone ("you don't have permission"): Darwin refuses to
  rename a directory its caller cannot write, and the engine seals the package
  read-only. The installer now restores the owner's write bit on the sealed
  root alone before moving it aside. The simulator's host file system had let
  the rename through.
- iOS and macOS: the engine's render and session threads ran at the default
  quality-of-service class, so the main thread waiting on the session's
  startup was a priority inversion and the frame work could land on efficiency
  cores. They are now user-interactive; decode and IO threads are utility.

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
