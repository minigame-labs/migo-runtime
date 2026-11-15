# Data handling

> This document is the authoritative English version. It is not translated,
> to avoid the legal risk of a translation drifting from the source text.

What this runtime does with data, for the security review that precedes
embedding it.

This is a factual description of the software, not a privacy policy. Migo is a
library inside your application: your users have a relationship with you, not
with us, and the privacy policy that governs them is yours. This document exists
so you can write that policy accurately.

The claims below can be checked from outside the binary: run a game with no
network policy set and capture the device's traffic, or read the per-package
SBOM attached to each release. A review that needs the source behind a claim can
be arranged under NDA — write to `security@minigame-labs.com`.

## The runtime does not phone home

There is no telemetry, no analytics, no crash reporting, no update check, and no
licence activation. Nothing is sent anywhere on start-up, on error, or on a
timer.

- **No engine-initiated requests.** Every outbound request originates in a
  content API call — `migo.request`, `migo.downloadFile`, `migo.uploadFile`,
  `migo.connectSocket`, audio streaming, remote image loading.
- **Errors go to you, not to us.** A panic, an ANR, a V8 heap limit or an
  execution timeout is reported through the host error callback, which on
  Android reaches `NativeExports.onError`. It is a callback into your process.
- **No network on the start-up path.** Creating the engine and a session makes
  no requests.

This is a design constraint rather than a current state of affairs. A runtime
sold on "you pin the version and audit the boundary" cannot also be one that
reports home, so the absence is deliberate and is meant to stay.

## What content can reach, and what gates it

Every capability below is refused unless your app allows it. The runtime does
not decide any of them.

| Capability | Gate |
|---|---|
| Network | **Nothing until you grant it.** One domain allowlist per channel — request, download (also remote images, audio streaming and asset prefetch), upload, WebSocket, TCP, UDP — set with `RuntimeConfig.Builder.setNetworkPolicy` on Android, `networkPolicyJSON` on Apple's `MigoGameView`, or `MigoSessionConfig.network_policy_json_utf8` through the C ABI. A host that sets none gets no network at all — and the OpenHarmony host and the Linux Qt kit's managed session take no policy yet, so content there reaches no network. Entries are domains, never IP addresses; plain `http://` and `ws://` and the local network — the other devices on the device's own networks, read from those networks rather than the address range — are off unless switched on; a request, download or upload never reaches the local network or the loopback, and nothing reaches the device itself by any of its addresses. Applied to every hop **including redirect targets**, on the channel the request was made on, so `allowed.com -> 302 -> blocked.com` is refused and a redirected download is still held to the download allowlist. A machine that routes HTTP through a proxy (`HTTP_PROXY` and the like) hands the proxy the domain, and the proxy resolves it: where an allowed domain then leads is that proxy's to decide. A URL content hands the app to show, share, play or decode — `previewImage`, `previewMedia`, a share's image, a video's source and cover, a video decoder's source — is held to the download allowlist the same way; the app or the system media stack then fetches it, resolving the domain and following its redirects itself. |
| Filesystem | Each game is confined to its own directories; `..`, absolute paths and symlinked archive entries are rejected. |
| Key-value storage | Per game, not per app: one title cannot read another's data, and `migo.clearStorage()` cannot clear another's. 10 MB total, 1 MB per value, enforced inside the SQLite transaction. |
| Camera, microphone, location, Bluetooth | Denied unless your `PermissionHandler` grants the scope. With no handler installed, all are denied. |
| Advertising | No ad SDK is linked. Whether an incentivised video was watched is decided by your ad SDK and passed through; the runtime cannot report a completed view on its own. |
| Payment, login | Transport only. Eligibility, settlement and risk control stay with you. |

Coverage of the permission gate is checked in CI by a contract that derives the
set of operations needing a check from the sources rather than from a list, so
an operation added later cannot quietly skip one.

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
CycloneDX SBOM is attached to each release alongside the AAR, the Apple SDK and
every C ABI package; the licence texts are in `NOTICE`.

Neither V8 nor Skia is a service. Nothing in the dependency tree contacts a
network endpoint on its own.

## What is not in scope

Content is yours, and so is what it does. A game running here can request the
network within your allowlist, write into its own storage, and use whichever
capabilities you granted. The runtime confines it; it does not audit it. If you
run third-party titles, review them.

Questions from a security review: `security@minigame-labs.com` (see
[SECURITY.md](SECURITY.md)).
