# Migo C ABI and Surface v1 candidate

These headers are a design candidate. They make Migo's planned low-level embedding contract reviewable from C11 and C++17. On Linux they are also callable: `scripts/build-linux-sdk.sh` produces `libmigo.so` and `libmigo.a` exporting exactly the `migo_*` set declared here, with pkg-config and CMake integration, and `scripts/build-android-c-host.sh` cross-compiles the same implementation as a static library for Android. Everywhere else the headers remain compile-only.

The public markers are intentional:

```c
MIGO_C_ABI_CANDIDATE  == 1     /* still a candidate everywhere */
MIGO_C_ABI_HAS_RUNTIME == 1    /* Linux, Android, Windows, OpenHarmony: a linkable runtime exists */
MIGO_C_ABI_HAS_RUNTIME == 0    /* every other target */
```

Windows joined that list once it met the same bar the others did, not when its
package first appeared: `scripts/build-windows-sdk.sh` produces `migo.lib` with
a CMake package, `scripts/test-windows-sdk-contract.sh` gates its export surface
and refuses to package without it, and the Win32 backend in
`engine/crates/capi/src/platform/windows.rs` attaches an `HWND` that renders
through ANGLE and receives pointer input. The macro had said 0 for some time
after all of that was true — a stale answer in the direction of understating,
which is the harmless direction, but still a wrong one.

A runtime existing is not the same as the ABI being frozen. Do not treat these headers as a stable SDK: the freeze blockers below are open, and the surface may still change.

OpenHarmony's runtime is a **static library**, packaged the same way Android's is and for the same reason: an OpenHarmony native module links its dependencies into its own `.so`. `scripts/build-ohos-sdk.sh` stages headers, `libmigo_capi.a`, a CMake package and a manifest for `aarch64` and `x86_64`, and `scripts/test-ohos-sdk-contract.sh` gates them — including that an external consumer links with every `migo_*` resolved, that the consumer binary uses the musl loader, and that the manifest claims no platform kind the library cannot attach. The surface is an ArkUI XComponent's `OHNativeWindow*`; `platforms/openharmony` is the host that drives it, and the whole chain — attach, content load, render, and a full touch lifecycle read back as pixels — was verified on an API 20 emulator before this macro was flipped. Only `x86_64` has run on a device: `aarch64` is built and gated but unverified, because that needs real HarmonyOS NEXT hardware.

Android's runtime is a **static library** a host links into its own `.so`, driven by `tests/c_host/android` — a NativeActivity with no Java of its own. It is now packaged for third-party NDK consumption: `scripts/build-android-sdk.sh` stages headers, `libmigo_capi.a`, a CMake package (`find_package(migo)`), and a per-ABI artifact manifest, and `scripts/test-android-sdk-contract.sh` verifies the export surface, the embedded snapshot, and that a `find_package(migo)` consumer (`tests/c_host/android-package-consumer`) links with every `migo_*` resolved. It deliberately ships a static library rather than a versioned shared object and CMake rather than pkg-config, because that is how an NDK host consumes a native dependency; those are not omissions. The `libmigo.so` the Java/JNI SDK ships is a different artifact and still exports no `migo_*` symbols.

## Header layout

- `migo.h` is the engine/session/Surface umbrella header.
- `types.h`, `capabilities.h`, `session.h`, and `surface.h` contain only standard C types.
- `capabilities.h` answers what the *linked library* supports; the `MIGO_C_ABI_*` macros above can only report what the headers were compiled against.
- `platform/*.h` contains strongly typed native target descriptors without including any platform SDK header.
- iOS declares `UIView*` and `CAMetalLayer*` descriptors. It previously declared none, on the assumption that the iOS backend would be an embeddable `WKWebView` container where WebKit owns the drawing surface. That assumption did not survive: the JIT boundary on iOS is drawn around the process, not around the engine, so content JavaScript runs inside WebKit's WebContent process while rendering returns to the host process -- which needs a host-owned Metal surface.

The generic descriptor is not a universal native-window union. It points to one typed Android, Win32, WinUI, macOS, iOS, X11, Wayland, or OpenHarmony descriptor, preserving the platform's best native integration model.

## Constant spelling

Integer constants here are suffixed literals (`5U`, `(1U << 0)`), not
`UINT32_C(5)`. Swift's Clang importer reads a macro's definition tokens rather
than its expansion and declines a function-like macro invocation -- so under the
conventional spelling, *every* constant in these headers, including
`MIGO_ABI_VERSION_CURRENT`, was unnameable from Swift. See the comment on
`MIGO_ABI_VERSION_1` in `types.h`. The forms are equivalent in C and C++, and
the literal, unlike a cast, still works in `#if`.

## Structure initialization and extension

Every caller-owned extensible structure starts with `struct_size` and `abi_version`. Callers zero-initialize the complete record, then set both fields explicitly:

```c
MigoSurfaceDescriptor descriptor = {0};
descriptor.struct_size = (uint32_t)sizeof(descriptor);
descriptor.abi_version = MIGO_ABI_VERSION_1;
```

A future implementation copies every retained structure, rejects an unsupported version or undersized required prefix, and ignores unknown trailing fields. Every reserved field must remain zero when supplied by a caller and must be written as zero by an implementation; this keeps the bytes available for a compatible future meaning. Descriptor pointers are borrowed only for the duration of the API call. Public enum-like values use fixed-width integer typedefs and numeric macros; the headers deliberately use neither C enums nor packing pragmas.

`MigoSurfaceDescriptor.platform_descriptor_size` deliberately duplicates the typed payload's `struct_size`. A receiver compares both before reading the payload, so the envelope cannot claim a larger readable range than the payload reports for itself.

## Surface generation and completion

The host chooses a non-zero generation that increases monotonically within a Session. Only one attachment is active at a time. Resize/DPI/color/presentation updates repeat the generation; a stale generation is rejected. Replacing a native target means detaching, waiting for release, then attaching with a newer generation.

The first successful attach fixes an immutable graphics platform identity for the Session. Replacement is supported only inside that domain: Android in the same process, X11 on the same server using the Session's private render connection, Wayland on the same `wl_display`, and HWND under the same ANGLE device. Switching backend or display returns `MIGO_ERROR_INVALID_STATE` synchronously before a Surface lease is published or a render command is enqueued; the existing Session remains unchanged and retryable.

`MigoSurfaceAttachment*` is a unique handle; callers must not create independently owned aliases. Retirement is a cold-path boundary and it is **asynchronous**, because the GPU cannot be made to forget a Surface synchronously: driver-side references outlive the call, and no return value can honestly claim otherwise.

`migo_surface_begin_detach` returning `MIGO_OK` consumes the attachment — its pointer is invalid, and no future GPU call or present references that generation — and produces a `MigoSurfaceRelease*` observer. A non-success result consumes nothing and changes nothing: `MIGO_ERROR_INVALID_ARGUMENT` for a NULL argument, `MIGO_ERROR_INVALID_STATE` when another Surface transition is running or the Session has no live host, `MIGO_ERROR_STALE_SURFACE` when the handle is not the active attachment.

**The host must not destroy its native resources when `begin_detach` returns.** It must wait until `migo_surface_release_query` reports `MIGO_SURFACE_RELEASE_RELEASED`. Destroying earlier is a use-after-free inside the driver, which the engine can neither detect nor prevent — the reference it would have to observe is not its own. `migo_surface_release_destroy` refuses with `MIGO_ERROR_INVALID_STATE` while the release is still pending, so the observer cannot be discarded while it is still the only thing that knows the answer.

The observer is level-triggered, so a release that completes before the first query is still reported; there is no edge to miss. It holds no Surface resource lease. A **released** observer may therefore remain valid after a later successful Session destruction. A pending observer cannot: `migo_session_destroy` refuses while any release is pending.

Detach can be marshalled to Migo-owned render/platform workers but cannot require an SDK-owned window or event loop, and it must not wait for another turn of the host dispatcher. Consequently a callback running on a single-threaded UI dispatcher can begin a detach reentrantly without blocking on its own queue.

**Destroying a Session does not detach for you.** `migo_session_destroy` returns `MIGO_ERROR_INVALID_STATE` while an attachment is still live, while a Surface transition is running, or while any release is still pending; the Session stays valid and the call can be retried. Teardown is therefore always the same three steps, in order: `migo_surface_begin_detach`, poll until `RELEASED`, then `migo_session_destroy`. Refusing is what keeps a host from tearing down a Session while the GPU still references its Surface — an error the engine can still catch, unlike the host destroying its own window early, which it cannot. No tombstone is retained per Surface recreation.

Losing a Surface is not detaching it. After a loss the attachment is still live and still owned by the caller, so the sequence above is unchanged.

## Native target ownership

| Target | Lifetime rule |
|---|---|
| Android `ANativeWindow*` | Migo acquires a strong reference before attach succeeds and releases its reference before the release observer reaches `RELEASED`. |
| OpenHarmony `OHNativeWindow*` | Migo takes its own native-object reference and releases it before `RELEASED`. |
| macOS `NSView*` / `CAMetalLayer*` | Migo retains the Objective-C object until retirement completes; the two target kinds remain distinct. |
| iOS `UIView*` / `CAMetalLayer*` | Same rule as macOS, and a separate pair of kinds. The four Apple records are byte-identical, so the kind is the only thing distinguishing a retain of a view from a layer the renderer will call `nextDrawable` on. |
| WinUI native SwapChainPanel interface | Migo keeps its own COM reference until retirement completes; this is not modeled as an HWND. |
| Win32 child `HWND` | Host-owned and valid until `RELEASED`; Migo neither destroys it nor owns the message loop. |
| X11 `Display*` + `Window` | `Display*` is borrowed synchronously during attach. Migo opens a private render connection and never closes or dispatches the host connection. The host connection and `Window` remain valid until `RELEASED`. |
| Wayland `wl_display*` + `wl_surface*` | Host-owned and valid until `RELEASED`; the host owns the role and dispatch loop. |

## Dispatcher, callbacks, and destruction

A callback record is copied according to `struct_size`; it is never borrowed. Callback configuration can be installed only once per Session and must be installed before the first Surface attach or transition to `MIGO_LIFECYCLE_RUNNING`; later calls return `MIGO_ERROR_INVALID_STATE`. This eliminates replacement races for queued callback function pointers and `user_data`. Any non-null user callback requires a non-null dispatcher. The dispatcher can be entered from an engine worker, must be thread-safe, and returns promptly. Returning `MIGO_OK` accepts exactly one task invocation; a rejection leaves ownership with Migo.

User callbacks execute only inside the dispatched task, with no Migo engine/session/attachment lock held. They may re-enter lifecycle, visibility, focus, detach, or destroy. Session destruction cancels queued user callbacks. A queued internal task may later run only to release its own storage; it cannot touch `user_data` after destruction. Reentrant destruction invalidates the Session immediately and permits only the current callback stack to unwind.

Successful `migo_session_destroy` and `migo_engine_destroy` calls consume and release their respective handles; those pointers are invalid afterward. All child Sessions must be destroyed before their Engine. Session destruction requests Host shutdown and transfers the exiting worker to the Engine; it does not self-join a callback that destroys its own Session.

Successful Engine destruction is the final thread-completion barrier. It joins every Migo-owned worker transferred by its Sessions without holding an Engine, Session, attachment, callback, or retirement lock. Calling it from one of those workers returns `MIGO_ERROR_INVALID_STATE` without consuming the Engine; the host must retry from another thread after the callback unwinds. After successful return, no Migo thread can access the host's native display/window resources, and the host may destroy those resources and unload the Migo library. Before it returns, neither action is valid even if every Surface release has reached `MIGO_SURFACE_RELEASE_RELEASED`: Surface release proves that generation is no longer used, while Engine destruction proves that no Migo code is still executing.

## Asynchronous operations

ABI v1 has two, and they are deliberately shaped differently.

**Surface release is observed authoritatively and may be reported as a wakeup.**
`migo_surface_begin_detach` hands back a `MigoSurfaceRelease*`; the host reads the
authoritative level with `migo_surface_release_query`. A host that installs the optional
`on_surface_released` callback may use that dispatched edge to schedule a query instead of
polling continuously. The callback is never proof that a native resource can be freed, and
older hosts remain correct without it. The observer handle is its own correlation token, so
no request ID is needed; the release cannot be cancelled because retirement is already
irreversible when `begin_detach` returns `MIGO_OK`. Once the query reports `RELEASED`, the
observer holds no Session or Surface lease and may outlive a later successful Session
destruction.

**Content load is reported.** `migo_session_load_content` starts evaluating content and
reports the outcome through `on_ready` or `on_error`. A Session loads content once, so at
most one completion can ever be outstanding. Its rules are contract, not description:

- **Correlation.** None is needed or offered. With one outstanding completion per Session,
  a request ID would be a constant. No entry point returns one, and hosts must not infer an
  ordering guarantee between Sessions from the order completions arrive.
- **Cancellation.** `migo_session_destroy` is the cancellation. There is no separate cancel
  entry point, and destruction is always a legal thing to do while a load is in flight.
- **Late completion.** A completion queued before destruction never runs after it. The
  Session is marked dead before `migo_session_destroy` returns; a task already handed to
  the dispatcher checks that when it runs and cancels itself, touching no `user_data`. A
  task the dispatcher rejects returns to the engine, which drops it — it is never run on an
  engine thread as a fallback.

These three are asserted by `a_destroyed_session_cancels_queued_callbacks` and
`a_rejected_dispatch_drops_the_task_instead_of_leaking_or_running` in
`engine/crates/capi/callbacks.rs`, so the contract above is checkable rather than merely
stated.

A *third* asynchronous operation, if it is a reported one, reopens the question. Request IDs,
a cancel entry point and a late-completion state machine are still not defined ahead of it:
fields can be added to a struct under `struct_size` negotiation, whereas invented ones cannot
be removed after they ship.

## Handle concurrency and destruction

Four handle kinds cross this ABI. Their rules differ, and conflating them is the most likely
way to get memory safety wrong:

| Handle | Uniqueness | Concurrency | Destruction |
|---|---|---|---|
| `MigoEngine*` | Unique, owned by the host | Entry points are thread-safe; the host serializes its own calls | `migo_engine_destroy` after every child Session is destroyed; successful return joins all Migo-owned workers |
| `MigoSession*` | Unique, owned by the host | Calls through one Session must be serialized by the host | `migo_session_destroy` consumes it, cancels queued callbacks, and transfers its exiting Host to the Engine, but **refuses** while an attachment is live, a transition is running, or a release is pending |
| `MigoSurfaceAttachment*` | Unique; aliases are forbidden | Serialized with its Session; only one is active at a time | Consumed by `migo_surface_begin_detach` only. Destroying the Session does not consume it — it fails instead |
| `MigoSurfaceRelease*` | Unique, owned by the host | Queryable from any thread the host serializes; holds no lease | `migo_surface_release_destroy`, and only once `RELEASED` |

`dispatcher_data` is not a handle and is never owned by the engine. It is copied out of the
callback record at install time and passed back verbatim on every dispatch. Because callbacks
can be installed only once per Session, it cannot be replaced while tasks referencing it are
queued — which is why it needs no lifetime protocol of its own. It must remain valid until
the owning Session is destroyed.

The one ordering that is not obvious: a **released** `MigoSurfaceRelease*` may outlive its
`MigoSession*`. Session destruction refuses while that observer is pending; after it reaches
`RELEASED`, destroying the Session does not invalidate it and a final query/destroy remains
well-defined. Any other cross-handle survival goes the other way — children never outlive
parents.

## Performance boundary

Descriptors are parsed and converted once during attach/update control operations. This contract adds no per-frame virtual dispatch, allocation, serialization, native-handle conversion, or callback hop. Presenter selection is fixed after attach; future platform backends remain free to use their best zero-copy graphics path.

## ABI v1 freeze blockers

The candidate cannot be declared stable until all of the following exist:

- performance-oriented batched pointer/touch, keyboard/text/IME, and gamepad contracts —
  **touch done** (`migo_session_send_touch`: batched, one copy at the boundary, no
  allocation, sharing the engine path Android already drives). **Desktop pointer done**:
  `migo_session_send_pointer_event` and `migo_session_send_wheel_event`. Until 2026-07-22 the
  runtime published `onMouseDown`/`onMouseMove`/`onMouseUp`/`onWheel` — names the common
  mini-game surface really defines, because mini-games run on PC clients of that platform — with the JS listener groups and
  their `_internalTrigger*` hooks present but *no producer anywhere*: no engine code called
  them and no host could, so content registering one was silently never called, on every
  platform, with no error. Deleting the listeners would not have been the fix; they are part
  of the common mini-game surface Migo clones, and removing them turns a silent no-op into a `TypeError`
  for PC mini-game content. The fix was the missing host channel, in the shape the soft keyboard
  already used: the engine exposes the capability, the host produces the events, and neither
  stream is synthesized from the other — an Android host sends touch, a desktop host sends
  the mouse, and a desktop host serving phone-first content may send both, because only the
  host knows which its content expects. Both records are pointer-free, so one layout serves
  LP64, LLP64 and ILP32 rather than the two halves every string-bearing record needs.
  `delta_mode` travels with the wheel deltas instead of being normalized to pixels, because
  converting a line-based delta needs the content's own line height, and an unrecognised
  mode is refused rather than assumed to be pixels.
  `scripts/test-input-trigger-producer-contract.sh` now fails if any published input listener
  loses its producer again; **soft keyboard done**: it is
  a capability the host supplies rather than one Migo has, so `on_show_keyboard` /
  `on_hide_keyboard` / `on_update_keyboard` install together on `MigoHostCallbacks` (all
  three or none — a host that can open a keyboard but not close it strands it on screen),
  and `migo_session_send_keyboard_event` carries input/confirm/complete/height back on the
  path Android already drives. The host's keyboard wins over the platform's, because
  Android's own accessor claims one unconditionally and reaches a JVM a pure-native host has
  not got. **physical keys done**: `migo_session_send_key_event` carries DOM `key`/`code`, a
  timestamp, the modifier state and `repeat` on the engine's existing `OnKeyDown`/`OnKeyUp`
  path. Not batched, unlike touch -- keys arrive at typing speed, so a batch API would be
  shape without a requirement. The host translates its platform keycodes into DOM values,
  because a portable runtime that accepted platform codes would have to carry a mapping per
  platform. Modifiers cannot be derived from `key`: a modified press still reports the
  character it produces, so without them content cannot tell `Ctrl+S` from `S`. They were
  appended after this record shipped and are therefore its optional tail -- a host built
  against the earlier header announces the smaller `struct_size`, and its absent fields read
  as zero, which is exactly what they mean. That is the first real exercise of the append
  rule on an input record rather than on `MigoHostCallbacks`, and it is covered on both
  sides: `tests/c_abi/old_client_contract.c` redeclares the pre-modifier shape and asserts it
  is still a byte-exact prefix, while `capi`'s own test hands the library a truncated buffer
  whose tail holds *valid* modifier bits -- garbage there would be caught by the known-bits
  check and surface as an error, whereas a plausible value would surface as content being
  told Ctrl and Alt are held when the host said nothing at all. **IME composition done**: the engine had no way to
  represent a preedit string, so this added both halves. The common mini-game platform has none, so the shape is the DOM
  `CompositionEvent` -- `compositionstart`/`update`/`end` carrying the whole current preedit,
  driven by `migo_session_send_composition_event`. It sits alongside the soft keyboard rather
  than replacing it: the keyboard reports committed text, composition reports what is still
  being typed, and content drawing its own text field needs both. **gamepad done**: the engine had none at all -- no JS
  API, no `HostCommand`, no dispatch -- so this added both halves. Mainstream mini-game platforms have no gamepad API, so
  the shape is the W3C one Migo replaces: `navigator.getGamepads()` plus
  `gamepadconnected`/`gamepaddisconnected`, driven by `migo_session_set_gamepad_connected`
  and `migo_session_send_gamepad_state`. The Web API is polled rather than evented, so a
  sample updates stored state instead of being dispatched, and `pressed` is carried rather
  than derived from `value` because a device picks its own threshold;
- asynchronous request IDs, cancellation races, and late-completion rules — **settled for
  v1**: v1 has exactly one asynchronous operation and it is single-shot per Session, so
  there is nothing to correlate and no request ID to carry. The rules are normative under
  "Asynchronous operations" below. A second asynchronous operation reopens this, and its
  shape is designed against that operation's requirements rather than guessed now;
- capability and supported-structure/version queries — **done**: `migo_query_capabilities`
  (`capabilities.h`) reports the accepted ABI version range and the attachable
  `MIGO_PLATFORM_*` kinds of the library actually linked. It is the one entry point that
  answers rather than rejects an unrecognised `abi_version`, because it is the call that
  resolves a version disagreement. The kinds it reports are the same fact the attach path
  enforces, not a second copy;
- Android and Linux implementations using this same contract — **Linux done, Android
  done**: `engine/crates/capi/platform/android.rs` implements the surface
  backend and the NativeActivity host in `tests/c_host/android` renders and takes touch
  on device, and rendering resumes after the app is backgrounded and returned to.
  **Multi-pointer delivery has run on a device** (2026-09-26, Mate 30 Pro, API 31):
  `scripts/verify-android-c-host-multitouch.sh` injects a real two-finger gesture and reads
  the pointer count that reached JS back from `touch-probe`'s pixels -- two held, magenta;
  the second lifted, green; both lifted, blue. A shell cannot produce that gesture
  (`sendevent` is refused by SELinux even though the shell account is in the `input`
  group, and `input motionevent` carries one pointer), so the check is an instrumentation
  APK, `tests/c_host/android-multitouch`, injecting through `UiAutomation.injectInputEvent`
  at the input dispatcher. It instruments itself rather than the host, because the host
  declares `android:hasCode="false"` and the system will not instrument a package with no
  code. It was shown to fail for a host that forwards one pointer (never magenta) and for
  one that marks every pointer removed on `POINTER_UP` (blue where green is due). What it
  cannot tell is *which* finger lifted, only how many remain; the ABI's batch conversion
  tests cover the flags. **Windows attaches an
  `HWND`** through `engine/crates/capi/src/platform/windows.rs`. That file did not exist
  until 2026-07-29: `platform/win32.h` declared `MigoWin32HwndDescriptor` and the
  `tests/c_abi` lanes pinned its layout for both pointer widths, so every gate agreed with
  every other gate while no implementation existed, and `migo_query_capabilities` reported
  no attachable kind at all. A published SDK loaded, resolved all 24 entry points, and
  could attach nothing. Header-to-header checks cannot see a missing implementation; the
  check that does is asking the built library what it supports, which
  `scripts/test-windows-sdk-contract.sh` now does and `scripts/build-windows-sdk.sh`
  refuses to package without;
- export lists, symbol/version tests, old-client/new-library tests, and per-target ILP32/LP64 layout lanes — **export list and symbol/version tests done** on every shipped target: each platform's SDK contract checks the built library's export surface -- Linux and Android against the per-product entry list `scripts/c-abi-entry-points.py` derives (`scripts/test-c-abi-entry-points-contract.sh`), OpenHarmony against its manifest, Windows from the PE export table -- where it began with `linux-x86_64` alone (`scripts/test-linux-sdk-contract.sh`); old-client/new-library lanes **done, inbound and outbound**; **layout lanes done for both pointer widths and both compiler families**. Every layout assertion in `tests/c_abi` is written twice, once per pointer width, but until 2026-07-21 only the LP64 half had ever been compiled — every lane ran on an LP64 host, so `#elif UINTPTR_MAX == UINT32_MAX` was dead source, and it had been wrong since the commit that appended `on_request_frame`: that commit updated the LP64 size and not the ILP32 one, and the soft-keyboard callbacks were then appended on top of the wrong base. `scripts/test-c-abi-surface-candidate.sh --ilp32` now compiles the lanes at `-m32` (freestanding, so it needs a multilib compiler but no 32-bit libc) and reports a skip loudly rather than passing silently when one is absent. `scripts/test-c-abi-msvc-lane.ps1` covers what no SysV compiler reaches: LLP64, MSVC's own C dialect under C11 with level-four warnings promoted to errors and strict conformance, `__cdecl` on x86, and the `__declspec(dllexport)`/`__declspec(dllimport)` branches of `MIGO_API`, which collapse to the GNU visibility attribute everywhere else. All four ABIs — LP64, LLP64, and ILP32 under both GCC and MSVC — agree. The append rule
  `MigoHostCallbacks` documents is now real: a caller's struct is copied, not
  reinterpreted, so a client compiled against an earlier header is accepted at its
  own `struct_size` and the fields it never had read as absent rather than as its
  neighbouring bytes. Smaller than the struct's documented minimum is
  `MIGO_ERROR_INVALID_ARGUMENT`; larger is `MIGO_ERROR_UNSUPPORTED_ABI`, because
  those bytes are a newer contract and ignoring them would be agreeing to
  semantics this library cannot deliver. Two lanes cover it:
  `tests/c_abi/old_client_contract.c` carries a previous header's shape and
  asserts it is still a byte-exact prefix of the current one — which catches a
  field *swap* that leaves the size and every pinned offset unchanged, and that
  the layout pins cannot see — while `capi`'s own tests cover the runtime
  behaviour against a truncated buffer with poisoned trailing bytes. The structs the
  library writes into (`MigoCapabilities`, `MigoSurfaceReleaseStatus`) have the
  mirror-image rule — write no more than the caller's `struct_size`, so a short old
  client is never overrun — and **outbound is now covered by the same two lanes**:
  `tests/c_abi/old_client_outbound_contract.c` independently redeclares each
  library-written struct and pins every field offset, catching a same-type field
  swap (e.g. `MigoCapabilities`'s adjacent `abi_version_min`/`abi_version_max`) that
  leaves `sizeof` and the header pins unchanged; and `migo_capi_abi`'s
  `output_prefix` tests cover the runtime behaviour against a *grown* struct whose
  appended field is poisoned, proving the library writes only the old client's prefix
  and leaves that field untouched. No library-written struct has appended a field
  yet, so the C lane's declared shape is the current one today; it becomes a true
  old-versus-new prefix check the moment one grows;
- Android/Linux compatibility and performance gates with no material regression — **Linux
  compatibility gate done**; **Android performance done**: against the Java SDK, the path every
  Android host embeds today, in one session on the same device and release (migo-bench
  `scripts/capi-ab.sh`; Mate 30 Pro, API 31, the published v0.9.9 AAR and C ABI package, three
  interleaved temperature-gated rounds of 60 s per game, each measured only after the screen was
  proved to be drawing), the C host's median fps, CPU and PSS stay inside the bounds fixed before
  any number existed -- fps at least 97%, CPU and PSS at most 105% of the Java SDK's: bunnymark 60
  vs 60 fps, 40% vs 46% CPU, 118.7 vs 121.0 MiB PSS; endless-runner 60 vs 60 fps, 39% vs 44% CPU,
  213.9 vs 213.1 MiB PSS (2026-09-26). Android compatibility has evidence but no gate: the bench's
  eight conformance contents (Canvas2D, readback, text, WebGL, surface geometry, screen fill, WASM)
  pass on the C host, run by hand. Linux performance is open;
- Android packaging for a third-party consumer — **done**: every release since v0.9.3
  publishes `migo-<version>-capi-android-{arm64,x86_64}.tar.gz`, built, gated and attested
  by CI. `scripts/build-android-sdk.sh` stages a CMake package (headers, `libmigo_capi.a`,
  `find_package(migo)`, per-ABI manifest); `scripts/test-android-sdk-contract.sh` verifies the
  22-symbol export surface, the freshness-gated snapshot identity, the complete staged-file
  hashes, and that a real `find_package(migo)` consumer links with every `migo_*` resolved. A
  versioned shared object and pkg-config are deliberately not provided — an NDK host links a
  static library through CMake, so those would be shape without a consumer. The V8 archive
  each ABI links is used only after `v8-materialise` has checked it against the committed
  component manifest (`engine/third_party/rusty_v8/<triple>/component-manifest.json`), and
  all eight snapshots are held current by `scripts/check-snapshot-freshness.sh`. On devices,
  the C host built from source passes `scripts/verify-android-c-host-multitouch.sh` --
  engine start, surface attach, content load, render and two-pointer input -- at the
  minimum and the target API level and one between: API 26 and API 34 (x86_64 emulators)
  and API 31 (Mate 30 Pro, arm64), 2026-09-26. The probe's first paint is a decoded image,
  loaded once before any WebGL context and once after, so both image decode paths are part of
  the check -- v0.9.8's package failed it, having no image decoder at all without the Java SDK.
  The published tarball's own bytes pass too: v0.9.9's arm64 package, linked through
  `scripts/build-android-c-host.sh --package`, on the Mate 30 Pro. The `-DANDROID_STL` matrix is measured rather than open: `c++_shared` links as-is,
  `c++_static` links with `-Wl,--allow-multiple-definition` (without it exactly six
  `std::runtime_error`/`std::logic_error` symbols collide — this library carries
  Chromium's libc++ inside V8's archive while the consumer brings the NDK's), and `none`
  does not link at all, because parts of the library were compiled against the NDK's
  libc++ headers. Both supported rows are asserted by
  `scripts/test-android-sdk-contract.sh`, and the shipped package README states the
  matrix and the flag — which matters more since this SDK stopped shipping a
  `libc++_shared.so` of its own, making `c++_static` the setting a host now reaches for.

The Linux artifact contract that is in place is described by
`dist/migo-linux-x86_64/share/migo/linux-x86_64-manifest.json`: target triple, CPU
baseline, glibc/GLIBCXX floor, path-independent sysroot identity, engine toolchain,
graphics contract, source/build provenance, dynamic dependencies, complete staged-file hashes,
and the exact V8 revision and GN arguments the archive was built from. The engine and V8
sysroot identities must match exactly.

Compile the current consumer contract with:

```sh
bash scripts/test-c-abi-surface-candidate.sh
```

This test compiles only small C/C++ fixtures. It does not build or link Migo, Android native code, or V8.

CI also compiles the same layout contract with the Android API 26 ARMv7 toolchain to exercise ARM ILP32 alignment. That lane is layout-only and does not publish or claim support for an `armeabi-v7a` runtime artifact; official Android artifacts remain `arm64-v8a` and `x86_64` unless a separate product decision adds ARMv7.
