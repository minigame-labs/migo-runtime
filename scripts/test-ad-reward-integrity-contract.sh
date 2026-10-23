#!/usr/bin/env bash
# The runtime must never be able to mint an ad reward.
#
# For incentivised video, `isEnded` on the `close` event is what content uses to
# decide whether to pay the player. It has to come from the host's ad SDK. If
# the runtime can produce a truthy `isEnded` on its own, the publisher hands out
# rewards for adverts nobody watched: they pay the reward and earn nothing. That
# was the state of this engine before the host-authoritative ad bridge landed --
# `01_ad.js` fired `close` with a hardcoded `{ isEnded: true }` on a timer.
#
# The regression is silent by nature. Every callback still fires, the flow still
# looks correct in a demo, and the damage only shows up in someone's revenue
# reconciliation weeks later. So it gets a source-level gate rather than relying
# on the behavioural tests alone (`tests/ad_reward_integrity.rs`), which can only
# cover the paths they were written for.
#
# The rule, applied to every embedded JS module:
#
#   `isEnded` may be assigned only `false`, or an expression that strict-compares
#   a host-supplied value to `true`. Anything else -- a `true` literal, a truthy
#   number, a string, a loose `!!` coercion -- is a violation.
#
# The gate also fails when its own anchors go missing, so deleting the sanitiser
# or the host event channel cannot turn it into a no-op.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

python3 - "$ROOT_DIR" <<'PY'
from __future__ import annotations

import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1]).resolve()
sys.path.insert(0, str(root / "scripts" / "lib"))
import runtime_ops  # noqa: E402

crates_dir = root / "engine/crates"
ad_js = crates_dir / "runtime-v8/src/ad/01_ad.js"
ad_scope_js = crates_dir / "runtime-v8/src/ad/99_global_scope.js"
ad_ops_rs = crates_dir / "runtime-v8/src/ad/mod.rs"
ad_service_rs = crates_dir / "shared/src/services/ad.rs"

for required in (crates_dir, ad_js, ad_scope_js, ad_ops_rs, ad_service_rs):
    if not required.exists():
        print(
            f"ERROR: {required} not found; this gate cannot check anything",
            file=sys.stderr,
        )
        sys.exit(1)

failures: list[str] = []

# ---------------------------------------------------------------------------
# 1. No embedded JS may originate a truthy `isEnded`.
#
# Matches an assignment to `isEnded` (object literal or `=`), and deliberately
# not a comparison: `=(?!=)` rejects `isEnded === true`, which is the read side.
# ---------------------------------------------------------------------------

ASSIGNMENT = re.compile(
    r"""["']?\bisEnded\b["']?\s*(?::|=(?!=))\s*(?P<value>[^,;}\n]+)"""
)
# The only approved producer shape: a strict comparison against a boolean true,
# i.e. the value came from somewhere else and was narrowed to a real boolean.
SANITISED = re.compile(r"===\s*true\b")
FALSE_LITERAL = re.compile(r"^false$")

sanitised_sites = 0
scanned_files = 0

for source_path in sorted(crates_dir.rglob("*.js")):
    if "/target/" in str(source_path):
        continue
    scanned_files += 1
    source = source_path.read_text(encoding="utf-8")
    for line_no, line in enumerate(source.splitlines(), start=1):
        stripped_comment = line.split("//", 1)[0]
        for match in ASSIGNMENT.finditer(stripped_comment):
            value = match.group("value").strip().rstrip(",;)")
            if FALSE_LITERAL.match(value):
                continue
            if SANITISED.search(value):
                sanitised_sites += 1
                continue
            failures.append(
                f"{source_path.relative_to(root)}:{line_no}: `isEnded` assigned "
                f"`{value}`; only `false` or a `=== true` narrowing of a "
                f"host-supplied value is allowed"
            )

if scanned_files == 0:
    failures.append(
        "no embedded JS was scanned; the gate would pass vacuously"
    )

# ---------------------------------------------------------------------------
# 2. Anti-vacuity: the sanitiser must still exist.
#
# Without this, deleting `_closePayload` -- and with it every `isEnded` write --
# would leave check 1 with nothing to reject, and the gate would go green on a
# runtime that no longer reports rewards at all.
# ---------------------------------------------------------------------------

if sanitised_sites == 0:
    failures.append(
        "no `isEnded` value is produced by a `=== true` narrowing anywhere in "
        "the embedded JS; the reward verdict is no longer forwarded from the "
        "host, so check 1 has nothing left to guard"
    )

# ---------------------------------------------------------------------------
# 3. The host event channel must exist end to end.
#
# The verdict is only trustworthy if it arrives over the inbound host bridge.
# Each of these anchors is one link in that chain; a missing link means the
# runtime is back to deciding rewards by itself.
# ---------------------------------------------------------------------------

ad_js_source = ad_js.read_text(encoding="utf-8")
ad_scope_source = ad_scope_js.read_text(encoding="utf-8")
ad_service_source = ad_service_rs.read_text(encoding="utf-8")

if not re.search(r"\bfunction\s+_internalOnAdEvent\s*\(", ad_js_source):
    failures.append(
        f"{ad_js.relative_to(root)}: `_internalOnAdEvent` is not defined; ad "
        "events have no inbound channel from the host"
    )

if "_internalOnAdEvent" not in ad_scope_source:
    failures.append(
        f"{ad_scope_js.relative_to(root)}: `_internalOnAdEvent` is not "
        "registered on the global scope, so the host bridge cannot resolve it"
    )

if not re.search(r"\bpub\s+trait\s+AdService\b", ad_service_source):
    failures.append(
        f"{ad_service_rs.relative_to(root)}: `AdService` is gone; there is no "
        "host-side seam for advertising"
    )

# The show path is what a host must be able to intercept: if `op_ad_show` stops
# existing, the JS side has no way to ask the host to play anything, and any
# `close` it reports afterwards cannot have come from a real advert.
#
# Read the op set from the extension's own `ops = [...]` declaration rather than
# from `fn` definitions: these ops are generated by a macro, so there is no
# literal `fn op_ad_show` to find, and a gate keyed on that spelling reports a
# violation against a perfectly wired bridge. The registration list is what
# deno_core actually exposes to JS, which is the property being asserted.
#
# Read through the engine's one parser of extension declarations
# (scripts/lib/runtime_ops.py), not a regex of this gate's own: the regex knew
# only `deno_core::extension!(`, and when the runtime's extensions moved into
# `main_runtime_extension!` (#619) it found no ops in a fully wired bridge.
registered_ops = {
    name for name, op in runtime_ops.registered_ops(root).items() if op.extension == "host_v8_ad"
}
if not registered_ops:
    failures.append(
        f"{ad_ops_rs.relative_to(root)}: the ad extension declares no `ops = [...]` "
        "list; the JS layer has no host bridge to call and this check cannot run"
    )

for required_op in ("op_ad_is_supported", "op_ad_show", "op_ad_create"):
    if registered_ops and required_op not in registered_ops:
        failures.append(
            f"{ad_ops_rs.relative_to(root)}: `{required_op}` is not registered in "
            f"the ad extension (registered: {sorted(registered_ops)}); the ad "
            "bridge is no longer wired to the host"
        )
    if not re.search(rf"(?<![A-Za-z0-9_]){re.escape(required_op)}(?![A-Za-z0-9_])", ad_js_source):
        failures.append(
            f"{ad_js.relative_to(root)}: `{required_op}` is never called; the "
            "embedded JS is not using the host bridge"
        )

# ---------------------------------------------------------------------------
# 4. The Java sink and the JS reader must agree on the wire format.
#
# Two producers write it: an ad object's handler (NativeExports), and the app
# itself for what no content call creates -- the cover advert's status,
# GameSession.reportSplashAdStatus, added 2026-10-12 and missed by this check,
# which turned master red on keys that were in fact sent.
#
# The ad event channel is JSON crossing Java -> JNI -> JS with nothing checking
# it: rename a key on one side and every callback still fires, the payload just
# arrives with the field missing. For `isEnded` that reads as "advert not
# watched" and silently stops paying players; for `event` it drops the callback
# entirely. Both sides are parsed from source here rather than listed, so a new
# event added later is covered without editing this file.
# ---------------------------------------------------------------------------

sink_java = root / (
    "platforms/android/library/src/main/java/com/migo/runtime/internal/NativeExports.java"
)
# The ad events the app itself raises rather than an ad object's handler: the
# cover advert's status, which no content call creates (reportSplashAdStatus),
# written as a JSON literal straight to the event channel.
session_java = root / "platforms/android/library/src/main/java/com/migo/runtime/GameSession.java"

if not sink_java.exists():
    failures.append(
        f"{sink_java} not found; the Java half of the ad event contract cannot be checked"
    )
else:
    # Commented-out code must not count as an implementation. Without this a
    # `// payload.put("adId", adId);` still satisfies the check, and the gate
    # goes green on a channel whose events can no longer be routed.
    def strip_comments(text: str) -> str:
        text = re.sub(r"/\*.*?\*/", "", text, flags=re.DOTALL)
        return "\n".join(line.split("//", 1)[0] for line in text.splitlines())

    sink_source = strip_comments(sink_java.read_text(encoding="utf-8"))

    # Java side: the event names it emits, and the payload keys it puts.
    emitted_events = set(re.findall(r'emit\(\s*adId\s*,\s*"([A-Za-z]+)"', sink_source))
    java_keys = set(re.findall(r'extra\.put\(\s*"([A-Za-z]+)"', sink_source))
    java_keys |= set(re.findall(r'payload\.put\(\s*"([A-Za-z]+)"', sink_source))

    # The app's own events: each `onAdEvent(sessionId, "<JSON literal>" ...)`,
    # its event name and every key it writes. No adId: they belong to no ad
    # object, which is how the runtime tells them apart.
    app_events: set[str] = set()
    if not session_java.exists():
        failures.append(
            f"{session_java} not found; the app's own ad events cannot be checked"
        )
    else:
        session_source = strip_comments(session_java.read_text(encoding="utf-8"))
        for call in re.finditer(r"onAdEvent\(\s*sessionId\s*,(?P<args>.*?)\);", session_source, re.DOTALL):
            literal = "".join(re.findall(r'"((?:\\.|[^"\\])*)"', call.group("args")))
            app_events |= set(re.findall(r'\\"event\\":\\"([A-Za-z]+)\\"', literal))
            java_keys |= set(re.findall(r'\\"([A-Za-z]+)\\":', literal)) - {"event"}
        if not app_events:
            failures.append(
                f"{session_java.relative_to(root)}: raises no ad event of the app's "
                "own; the cover advert's status is gone (or this check stopped matching it)"
            )

    # JS side: the event names it routes, and the payload fields it reads.
    handled_events = set(re.findall(r'case\s+"([A-Za-z]+)":', ad_js_source))
    handled_app_events = set(re.findall(r'event\.event\s*===\s*"([A-Za-z]+)"', ad_js_source))
    js_fields = set(re.findall(r"\bevent\.([A-Za-z][A-Za-z0-9_]*)", ad_js_source))

    for name in sorted(app_events - handled_app_events):
        failures.append(
            f"the app raises ad event `{name}` but the runtime handles no such "
            f"event ({ad_js.relative_to(root)}); it would be dropped in silence"
        )

    if not emitted_events:
        failures.append(
            f"{sink_java.relative_to(root)}: no ad events are emitted; the Java "
            "half of the channel is gone (or this check stopped matching it)"
        )
    if not handled_events:
        failures.append(
            f"{ad_js.relative_to(root)}: no ad events are handled; the JS half of "
            "the channel is gone (or this check stopped matching it)"
        )

    for name in sorted(emitted_events - handled_events):
        failures.append(
            f"the host emits ad event `{name}` but the runtime handles no such "
            f"event ({ad_js.relative_to(root)}); it would be dropped in silence"
        )

    # Keys the runtime reads out of an event payload, minus the routing fields it
    # supplies itself. Everything left has to be something the host actually puts.
    ROUTING_FIELDS = {"adId", "event"}
    for name in sorted(js_fields - ROUTING_FIELDS - java_keys):
        failures.append(
            f"{ad_js.relative_to(root)}: reads `event.{name}` from ad payloads, "
            f"but neither {sink_java.relative_to(root)} nor the app's own events in "
            f"{session_java.relative_to(root)} put that key; the field would always "
            "arrive undefined"
        )

    for name in sorted(ROUTING_FIELDS - java_keys):
        failures.append(
            f"{sink_java.relative_to(root)}: ad payloads must carry `{name}`; "
            "without it events cannot be routed to an ad object"
        )

    # -----------------------------------------------------------------------
    # 5. Every ad command must settle a request it cannot forward.
    #
    # A full-profile Android session always installs an ad service, so content
    # takes the hosted path whether or not the embedder registered a handler:
    # "hosted with no handler" is the ordinary state of an integration in
    # progress. Three of the six commands used to resolve the handler with a
    # bare map lookup and `return`, so a hide() was dropped without a word and
    # a custom ad's onHide never came -- and that lookup also skipped the
    # dead-session cleanup the resolver performs.
    #
    # What each command settles as is checked behaviourally by
    # AdSettlementTest. This check exists because *which resolver an entry
    # point calls* is not observable from there: NativeExports holds
    # android.os.Handler statics, so a host JVM cannot load it, and the module
    # has no Robolectric.
    # -----------------------------------------------------------------------

    def blank_strings(text: str) -> str:
        # Same length, no contents: brace matching below must not trip over a
        # `"{}"` literal, and offsets have to stay comparable.
        return re.sub(
            r'"(?:\\.|[^"\\\n])*"',
            lambda m: '"' + " " * (len(m.group(0)) - 2) + '"',
            text,
        )

    def body_after(text: str, brace_index: int) -> str:
        depth = 0
        for index in range(brace_index, len(text)):
            if text[index] == "{":
                depth += 1
            elif text[index] == "}":
                depth -= 1
                if depth == 0:
                    return text[brace_index : index + 1]
        return ""

    scan_source = blank_strings(sink_source)

    entry_points = list(
        re.finditer(
            r"public\s+static\s+void\s+(ad[A-Z]\w*)\s*\(\s*int\s+sessionId\s*,"
            r"\s*String\s+requestJson\s*\)\s*\{",
            scan_source,
        )
    )
    if len(entry_points) < 6:
        failures.append(
            f"{sink_java.relative_to(root)}: found {len(entry_points)} ad entry "
            "point(s), expected at least the six mini-game ad commands; this check "
            "stopped matching them and would pass vacuously"
        )

    for match in entry_points:
        name = match.group(1)
        body = body_after(scan_source, match.end() - 1)
        if not body:
            failures.append(
                f"{sink_java.relative_to(root)}: could not read the body of "
                f"`{name}`; the routing check cannot run"
            )
            continue
        if "adHandlerOrSettle(" not in body:
            failures.append(
                f"{sink_java.relative_to(root)}: `{name}` does not resolve its "
                "handler through `adHandlerOrSettle`, so a request it cannot "
                "forward is dropped instead of settled and content waits forever"
            )
        if "sAdHandlers.get(" in body:
            failures.append(
                f"{sink_java.relative_to(root)}: `{name}` reads `sAdHandlers` "
                "directly; that lookup skips both the settlement and the "
                "dead-session cleanup"
            )

    resolver = re.search(
        r"private\s+static\s+AdHandler\s+adHandlerOrSettle\s*\([^)]*\)\s*\{",
        scan_source,
    )
    if resolver is None:
        failures.append(
            f"{sink_java.relative_to(root)}: `adHandlerOrSettle` is gone; the "
            "entry points above have nothing to route through"
        )
    elif "settleWithoutAdvert(" not in body_after(scan_source, resolver.end() - 1):
        failures.append(
            f"{sink_java.relative_to(root)}: `adHandlerOrSettle` no longer "
            "settles the request when no handler is registered, so routing "
            "through it buys content nothing"
        )

# ---------------------------------------------------------------------------

if failures:
    print("FAIL: ad reward integrity contract", file=sys.stderr)
    for failure in failures:
        print(f"  - {failure}", file=sys.stderr)
    sys.exit(1)

print(
    f"PASS: ad reward integrity contract "
    f"({scanned_files} embedded JS files scanned, "
    f"{sanitised_sites} host-narrowed `isEnded` site(s))"
)
PY
