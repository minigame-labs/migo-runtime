# Data handling

> This document is the authoritative English version. It is not translated,
> to avoid the legal risk of a translation drifting from the source text.

What this runtime does with data, for the security review that precedes
embedding it.

This is a factual description of the software, not a privacy policy. Migo is a
library inside your application: your users have a relationship with you, not
with us, and the privacy policy that governs them is yours. This document exists
so you can write that policy accurately.

Statements below are claims about the source in this repository, which is
available for you to check. Where a claim is checkable by grep, the place to
look is named.

## The runtime does not phone home

There is no telemetry, no analytics, no crash reporting, no update check, and no
licence activation. Nothing is sent anywhere on start-up, on error, or on a
timer.

- **No engine-initiated requests.** Every outbound request originates in a
  content API call — `migo.request`, `migo.downloadFile`, `migo.uploadFile`,
  `migo.connectSocket`, audio streaming, remote image loading. They live in
  `engine/crates/runtime-v8/src/network/` and
  `engine/crates/audio/src/streaming.rs`.
- **Errors go to you, not to us.** A panic, an ANR, a V8 heap limit or an
  execution timeout calls `HostNotifier::notify_error`, which on Android reaches
  `NativeExports.onError`. It is a callback into your process. See
  `engine/crates/core/src/services/platform.rs`.
- **No network on the start-up path.** `core/src/runtime/host.rs` and
  `runtime-v8/src/host_runtime.rs` make no requests.

This is a design constraint rather than a current state of affairs. A runtime
sold on "you pin the version and audit the boundary" cannot also be one that
reports home, so the absence is deliberate and is meant to stay.

## What content can reach, and what gates it

Every capability below is refused unless your app allows it. The runtime does
not decide any of them.

| Capability | Gate |
|---|---|
| Network | **Nothing until you grant it.** One domain allowlist per channel — request, download (also remote images, audio streaming and asset prefetch), upload, WebSocket, TCP, UDP — set with `RuntimeConfig.Builder.setNetworkPolicy` on Android or `MigoSessionConfig.network_policy_json_utf8` through the C ABI. A host that sets none gets no network at all. Entries are domains, never IP addresses; plain `http://` and `ws://` and the local network (RFC 1918, unique-local) are off unless switched on, and a request, download or upload never reaches the local network or the loopback. Applied to every hop **including redirect targets**, on the channel the request was made on, so `allowed.com -> 302 -> blocked.com` is refused and a redirected download is still held to the download allowlist. A machine that routes HTTP through a proxy (`HTTP_PROXY` and the like) hands the proxy the domain, and the proxy resolves it: where an allowed domain then leads is that proxy's to decide. `services/src/network/gate.rs` |
| Filesystem | Each game is confined to its own directories; `..`, absolute paths and symlinked archive entries are rejected. `runtime-v8/src/file/fs.rs`, `io/src/zip_extract.rs` |
| Key-value storage | Per game, not per app: one title cannot read another's data, and `migo.clearStorage()` cannot clear another's. 10 MB total, 1 MB per value, enforced inside the SQLite transaction. `runtime-v8/src/storage/mod.rs` |
| Camera, microphone, location, Bluetooth | Denied unless your `PermissionHandler` grants the scope. With no handler installed, all are denied. `runtime-v8/src/permission.rs` |
| Advertising | No ad SDK is linked. Whether an incentivised video was watched is decided by your ad SDK and passed through; the runtime cannot report a completed view on its own. `shared/src/services/ad.rs` |
| Payment, login | Transport only. Eligibility, settlement and risk control stay with you. |

Coverage of the permission gate is checked in CI by
`scripts/test-permission-coverage-contract.sh`, which derives the set of
operations needing a check from the sources rather than from a list, so an
operation added later cannot quietly skip one.

## Where data sits

All of it is inside your app's sandbox; the runtime creates nothing outside the
directories you supply.

| Data | Location | Lifetime |
|---|---|---|
| Game code | `filesDir/migo/games/<gameId>/code` | Until you delete it |
| Game key-value storage | `filesDir/migo/games/<gameId>/user_data/kv_storage` | Until you delete it |
| Game cache, blob URLs | `cacheDir/migo/games/<gameId>` | System may clear |
| V8 code cache | The code-cache directory you configure | Rebuilt if removed |

Deleting a game's directory removes everything that game stored. `GamePaths`
exposes the paths so you can do it.

## Third parties inside the binary

V8 and Skia, both BSD-3-Clause, plus their transitive Rust dependencies. A
CycloneDX SBOM is generated per release (`scripts/generate-sbom.sh`) and
attached to it; the licence texts are in `NOTICE`.

Neither V8 nor Skia is a service. Nothing in the dependency tree contacts a
network endpoint on its own.

## What is not in scope

Content is yours, and so is what it does. A game running here can request the
network within your allowlist, write into its own storage, and use whichever
capabilities you granted. The runtime confines it; it does not audit it. If you
run third-party titles, review them.

Questions from a security review: `security@minigame-labs.com` (see
[SECURITY.md](SECURITY.md)).
