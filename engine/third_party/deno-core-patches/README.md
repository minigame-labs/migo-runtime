# deno_core embedder patch

`UPSTREAM` pins the published crate and SHA-256. The vendored directory is the
entire published crate plus the ordered patches here. Verify it with
`bash scripts/test-deno-core-vendor-contract.sh` from the repository root.

`0001-opt-in-zero-delay.patch` adds a seventh, optional `allowZero` argument to
`core.createTimer`. Only one-shot callers explicitly passing `true` can use zero
milliseconds. All existing upstream calls retain the one-millisecond minimum;
repeat timers keep it even when opted in. Migo first schedules a zero-period
interval as a one-shot and subsequently uses a one-millisecond repeating timer.

It also checks destruction before rearming a repeating handle after its callback.
Cancelling an interval inside that callback must not leave a reactor wakeup; the
paused-clock regression proves the extra wakeup occurs without this check.

The patch uses the existing timer buckets, heap, reactor, references, cancellation
and statistics. Migo owns only mini-game policy: IDs, nesting clamp, lifecycle,
quota and callback context. It never reads private timer-handle properties.

Changing this patch changes embedded core JS and requires snapshot regeneration.
Do not replace the V8 150 archive with a previous V8 archive to make a build pass.

`0002-embedder-task-checkpoint.patch` exposes the event loop's existing tick and
microtask drain as `JsRuntime::perform_microtask_checkpoint`. It reports pending
exceptions and termination, dispatches no timers or I/O, and lets the host finish
one logical input/script/hook task before receiving the next. Callers release
callback scopes and op-state borrows first and cover the drain with the watchdog.

0003-bounded-serializer.patch adds optional maxBytes to core.serialize.
It requires forStorage: true without transferred buffers, so rejection cannot
leak out-of-band store entries or detach caller buffers. The V8 Rust delegate
caps allocation growth at 2 * maxBytes + 128, and core also enforces the exact
encoded byte limit. Other serializer, clone and broadcast callers preserve
their existing default. This is a codec prerequisite; Worker transport and its
aggregate queue/staging admission still need their own implementation.

0004-owned-storage-codec.patch factors the existing JS ops into shared internal
functions and exports serialize_for_storage / deserialize_for_storage for Rust
embedders. It adds no second delegate or type algorithm. A ready Worker can
reserve its queue, serialize directly to owned Rust bytes, and decode those bytes
at the receiver without intermediate JS wire buffers waiting for GC. The
serializer returns None when V8 has rethrown an exception/termination; callers
must publish nothing and immediately return, releasing their reservation.

0005-android-errno.patch gives uv_compat's tty module bionic's errno accessor,
`__errno()`. Upstream covers macOS and glibc/musl Linux only and refuses to
compile anywhere else, which stopped every Android build of the crate. Rust
only: no core JS changes.

0006-embedded-extension-sources.patch embeds the `esm`/`js` sources an
extension declares, as deno_core did before 0.4: `include_js_files!` takes its
`included` mode instead of `loaded`. Upstream records each file by its absolute
path on the build machine and reads it when a runtime boots from source, which
only works on that machine. Off it, every Worker (the release ships no Worker
snapshot), any build whose host snapshot is stale, and the snapshot generator
running on its target device aborted with "Failed to initialize a JsRuntime: No
such file or directory". A snapshot build still carries none of the main
runtime's JS: Migo leaves those sources off the extension
(`main_runtime_extension!`). Rust only: no core JS changes.
`runtime-v8`'s `tests::extension_sources` fails without it.
