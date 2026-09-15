# ProbeApp

The authoritative contracts are [`contracts/apple/`](../../../contracts/apple): the
deployment floor with its per-lane minimums, and the profile-selection policy.
Each carries its reasoning inline and each has a gate that fails when a consumer
drifts from it.

The maintainer plan behind them is deliberately **not** in this repository --
`docs/` is gitignored, so a link into it resolves for nobody who clones this.
What matters for anyone reading the code is in the contracts above and in the
comments here.

G0 must select a winner, or an explicit capability-specific choice, only from
real evidence on the current minimum OS and representative devices. It must
not report a topology as successful or failed in advance.

| G0 probe arm | Alternatives to measure | Required evidence |
|---|---|---|
| JavaScript agent | Window; Dedicated Worker | Conformance, synchronous API behavior, memory, CPU, latency, and lifecycle under the same workload |
| WebKit-to-app transport | custom `WKURLSchemeHandler`; loopback WebSocket on literal `127.0.0.1`; hybrid only after directional-bottleneck evidence | Secure-context/isolation, CORS, actual payload/API/body type and POST body delivery, copies, cancellation, streaming, authentication, and bounded backpressure |
| Frame clock | feature-detected Worker rAF when available; Window rAF relay; host `CADisplayLink` relay | Input-to-present latency, p99 jitter, missed-vsync rate, CPU, and behavior during backgrounding/occlusion |
| `WKWebView` host shape | attached visible; transparent overlay; 1×1; off-screen; occluded | WebContent liveness, rendering correctness, lifecycle, memory, occlusion behavior, and App Review/public-API compliance |

The transport arm must record that WebKit bug
[191362](https://bugs.webkit.org/show_bug.cgi?id=191362) is officially
**RESOLVED FIXED**. It cannot be used as a pre-written custom-scheme failure.
The probe still tests the real payload and API/body type on each target
OS/device, including secure context, isolation, CORS, copying, body delivery,
cancellation, streaming, and backpressure. No arm is presumed to win.

**Agent-dependent handoff invariant.** If Worker wins, a bounded transferable
`ArrayBuffer` ping-pong/pool carries frame bytes to Window. A small same-
WebContent-process Worker↔Window `SharedArrayBuffer` mailbox carries
synchronization only and never carries frame bytes. If Window wins, there is no
Worker relay.

**Full matrix.** First isolate variables with a 32 KiB / 60 Hz run. Only
winning combinations run the full matrix: 4 KiB, 32 KiB, 256 KiB, 1 MiB, and
4 MiB payloads at 30/60/120 Hz where hardware allows; a steady 30-minute run;
burst traffic; two-credit backpressure; resource upload concurrent with frames;
small and maximum synchronous replies; cancellation; background/foreground;
occlusion; WebContent kill; memory pressure; and cold/warm start. Every packet
has a deterministic correctness hash and receipt.

The result records the complete matrix, selected capability key(s), raw traces,
and reasons for rejection. Any hybrid selection requires measured directional
bottleneck evidence. Product code remains unchanged until the G0 record is
reviewed.

---

## What is implemented here today: measurement gate 1

The measurement order in the plan is itself a conclusion, and its first gate is
the **capability gate**: release-signed, on real devices, across the target OS
minors, **with no renderer and no validator**. It answers which of the
architectures above a device can host at all, and every later gate draws its
candidates from that answer. Benchmarking an arm the device cannot run
benchmarks whatever ran instead.

That gate is what this app runs. Nothing else here is implemented yet, and the
performance arms deliberately are not: a transport measured before the
capability gate has cut the candidate set is a transport measured against a
fallback.

### Where the code is, and why it is not in this target

| | |
|---|---|
| Record types, closed enums | `platforms/apple/core/Sources/MigoProbeCore` |
| Loopback origin, custom scheme, probe page, orchestration | `platforms/apple/core/Sources/MigoProbeHarness` |
| The app: a window, a web view, two segmented controls | `platforms/apple/ProbeApp/Sources` |

Almost nothing is in the app target on purpose. `platforms/apple/core` is the
package that resolves with nothing fetched and nothing built, so the free macOS
runner builds and tests it on every pull request — including the WebSocket
framing, the scheme handler's body reading, and the check that the probe scripts
answer every capability the contract declares. An Xcode target is code no lane
compiles until somebody opens Xcode, so the rule is that only what genuinely
needs a device goes in one.

Gate 1 links no engine. That is not an economy, it is the definition: the gate
runs with no renderer and no validator, so there is nothing for it to link.

### What one run produces

Two records — one per origin, `loopback` and `custom_scheme` — in the shape
`contracts/apple/capability-probe.schema.json` requires. Half the questions are
answered by the origin rather than the device (secure context, cross-origin
isolation, and therefore whether `SharedArrayBuffer` constructs at all), so one
record per device would hold two answers under one key.

They are written to the app's Documents directory, which `UIFileSharingEnabled`
exposes, so an operator pulls them off with the Files app or `devicectl` without
attaching a debugger.

### The two answers the operator has to give

Both segmented controls default to "unknown", and unknown is recorded as
unknown:

- **Lockdown Mode.** A24: it disables JIT for web content, so "WebContent has
  JIT" is a device state and not an admission invariant. iOS exposes no public
  query, so the person holding the phone declares it.
- **The local-network alert.** No API reports whether the system presented one.
  What the process can see is that traffic flowed, and traffic flows both when
  nobody was asked and when the operator tapped Allow.

Defaulting either to the common case is how a Lockdown-Mode device's answers get
filed as a JIT device's.

### One command

`scripts/run-apple-probe.sh` builds, installs, launches, waits for the records and
runs the admission. It is the supported way to run this gate; the manual steps below
are what it does, kept for when one of them needs to be debugged on its own.

```sh
# A device. Produces evidence, and therefore demands both attestations -- the two
# answers no API reports. Without them the record says `not_probed`, the loopback
# origin requires the alert answer, and the admission then reports every loopback
# candidate as unmeasured, which reads as though loopback had been ruled out.
scripts/run-apple-probe.sh --device <udid> --lockdown off --local-network-prompt none

# A simulator. Proves the harness and never the platform: the records land outside
# the evidence directory and no admission is computed for them.
scripts/run-apple-probe.sh --simulator

# What a run would do, on any machine, without an Apple toolchain.
scripts/run-apple-probe.sh --device <udid> --lockdown off \
    --local-network-prompt none --dry-run
```

A device build needs a signing identity: set `MIGO_PROBE_TEAM` to a team id. A free
Apple ID is enough -- this target declares no entitlements, and the JIT that A24 is
about belongs to WebKit's own process, not to this app. The team id is the `OU` of
the signing certificate, which is **not** the identifier printed in the certificate's
common name:

```sh
security find-identity -v -p codesigning          # names the identity
security find-certificate -c '<that name>' -p | openssl x509 -noout -subject
# subject= /UID=.../CN=Apple Development: you@example.com (XXXXXXXXXX)/OU=TEAMIDHERE/...
```

A free team also has no provisioning profile for `dev.migo.probe` until one is asked
for, so a device build passes `-allowProvisioningUpdates`; `run-apple-probe.sh` does
that for you.

### The steps underneath

```sh
# The engine-free package, natively and for iOS. This is the half that has
# tests, and the half CI runs on every pull request.
cd platforms/apple/core && swift test

# The app, for the simulator. Answers everything except JIT, which the
# simulator cannot answer at all -- it runs the host's JavaScriptCore on the
# host's CPU. The decision tools exclude simulator records for exactly that
# reason, so a simulator run is a test of the harness and never evidence.
cd platforms/apple/ProbeApp
xcodebuild -project MigoProbe.xcodeproj -scheme MigoProbe \
  -sdk iphonesimulator -destination 'generic/platform=iOS Simulator' build

# The device build needs a signing identity, and -allowProvisioningUpdates so a
# free team can mint the profile and register the device; everything else about
# it is the same.
xcodebuild -project MigoProbe.xcodeproj -scheme MigoProbe \
  -destination 'generic/platform=iOS' -allowProvisioningUpdates \
  DEVELOPMENT_TEAM=<team> build
```

`Info.plist` carries `NSAllowsLocalNetworking` and **not**
`NSAllowsArbitraryLoads`, and no Bonjour key. G0.3's list is part of what this
gate exists to check, and a probe app that needed an exemption the product
cannot ship would have measured the exemption.
