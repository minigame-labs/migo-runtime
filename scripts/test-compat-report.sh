#!/usr/bin/env bash
# The compatibility report says of each API what the contracts say, and reads the engine's own error lines.
#
# scripts/lib/compat_report.py turns one run's log into the answer a CP gets about its game: what it
# called, whether this build answers it on the platform, and what failed. Two ways it can be wrong without
# looking wrong:
#
#   * It re-derives a rule the contracts already decide. A first version judged a host-app API "owned" by
#     reading the API's needs against the host-owned services -- missing callback needs and needs the
#     platform has a default for, which the matrix generator counts. Readiness is read from the matrix
#     (`not_ready`) now, and the checks below hold each verdict to the cell it comes from.
#   * It greps for error shapes the engine does not emit. prescreen-run.sh has matched
#     "module evaluation failed" since it was written; until the engine fix that came with this report,
#     nothing logged it. The error lines below are the engine's, copied from real runs, so a pattern that
#     stops matching what the engine writes goes red here.
#
# The verdict checks run on synthetic contracts, so a contract that improves (an API gets verified) does
# not turn this red; one sweep over the real contracts checks every API there gets a verdict.
#
# Host-only: python3.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

python3 - "$ROOT" <<'PY'
import sys

sys.path.insert(0, sys.argv[1] + "/scripts/lib")
import compat_report as cr

failures = 0


def check(name, condition, detail=""):
    global failures
    if condition:
        print(f"  ok   {name}")
    else:
        failures += 1
        print(f"  FAIL {name}{': ' + str(detail) if detail else ''}")


contracts = {
    "matrix": {
        "notOffered": {"android": "unsupported", "needs": ["unpublished"], "not_ready": []},
        "notYet": {"android": "missing", "needs": [], "not_ready": ["android"]},
        "breaks": {"android": "engine", "needs": [], "not_ready": ["android"]},
        "hostReady": {"android": "host", "needs": ["channel:auth.login"], "not_ready": []},
        "hostNotReady": {"android": "host", "needs": ["callback:on_camera"], "not_ready": ["android"]},
        "answeredUnverified": {"android": "default", "needs": [], "not_ready": ["android"]},
        "answeredVerified": {"android": "engine", "needs": [], "not_ready": []},
        "hostReadyButFails": {"android": "host", "needs": ["channel:ad.show"], "not_ready": []},
        "updateHostApp": {"android": "host", "needs": ["channel:ecosystem.call"], "not_ready": []},
        "readyElsewhere": {"android": "engine", "ios": "engine", "needs": [], "not_ready": ["ios"]},
    },
    "decisions": {"unsupported": {"notOffered": {"reason": "needs a vision runtime"}}, "host_owned": ["auth", "ad"]},
    "conformance": {
        "measured": {"date": "2026-10-15"},
        "apis": {
            "breaks": {"status": "failing", "failing": ["api-spec/breaks/succeeds"]},
            "answeredUnverified": {"status": "present"},
            "answeredVerified": {"status": "verified"},
            "hostReadyButFails": {"status": "failing", "failing": ["api-spec/hostReadyButFails/shows"]},
            "readyElsewhere": {"status": "verified"},
        },
    },
}

expected = {
    "notOffered": "unsupported",
    "notYet": "missing",
    "breaks": "failing",
    "hostReady": "host-owned",
    "hostNotReady": "host-no-default",
    "answeredUnverified": "unverified",
    "answeredVerified": "verified",
    "hostReadyButFails": "failing",
    "readyElsewhere": "verified",
    "setTimeout": "outside",
}
for name, verdict in expected.items():
    got = cr.judge(name, "android", contracts)["verdict"]
    check(f"{name} is {verdict}", got == verdict, f"got {got}")
check("an unsupported API carries the decision's reason",
      cr.judge("notOffered", "android", contracts).get("reason") == "needs a vision runtime")
check("a failing API carries its failing checks",
      cr.judge("breaks", "android", contracts).get("failing") == ["api-spec/breaks/succeeds"])
check("readiness is per platform: what is not ready on ios is unverified there, and verified on android",
      cr.judge("readyElsewhere", "ios", contracts)["verdict"] == "unverified")

log = [
    "2026-10-18T20:23:22Z  INFO runtime_v8::console: __MIGO_COMPAT__{\"kind\":\"start\",\"version\":1}",
    "\x1b[2m2026-10-18T20:23:22Z\x1b[0m \x1b[32m INFO\x1b[0m runtime_v8::console: __MIGO_COMPAT__"
    "{\"kind\":\"call\",\"surface\":\"wx\",\"api\":\"updatePlatformApp\",\"migo\":\"updateHostApp\"}",
    "I/[migo]  (12345): __MIGO_COMPAT__{\"kind\":\"call\",\"surface\":\"wx\",\"api\":\"breaks\"}",
    "__MIGO_COMPAT__{\"kind\":\"call\",\"surface\":\"migo\",\"api\":\"answeredVerified\"}",
    "__MIGO_COMPAT__{\"kind\":\"call\",\"surface\":\"wx\",\"api\":\"setTimeout\"}",
    "__MIGO_COMPAT__{\"kind\":\"absent\",\"surface\":\"wx\",\"api\":\"notOffered\"}",
    "__MIGO_COMPAT__{\"kind\":\"absent\",\"surface\":\"wx\",\"api\":\"cloud\"}",
    "__MIGO_COMPAT__{\"kind\":\"installed\",\"surface\":\"wx\",\"api\":\"shim\"}",
    "__MIGO_COMPAT__{\"kind\":\"gl-extension\",\"context\":\"webgl2\",\"name\":\"EXT_color_buffer_float\",\"supported\":false}",
    "__MIGO_COMPAT__{\"kind\":\"gl-error\",\"context\":\"webgl\",\"code\":1282}",
    "__MIGO_COMPAT__{\"kind\":\"unrecorded\",\"surface\":\"web\",\"reason\":\"accessor\"}",
    "__MIGO_COMPAT__{\"kind\":\"first-frame\",\"ms\":190.7}",
    "__MIGO_COMPAT__{not json",
    "a line that only mentions __MIGO_COMPAT__ in passing",
]
# The engine's own lines, from real runs on the Linux player (2026-10-19).
engine_ready = [
    "\x1b[2m2026-10-18T21:22:20Z\x1b[0m \x1b[32m INFO\x1b[0m migo_core::runtime::host: "
    "[Host 1] evaluate_module('player-demo', 'game.js'): 191.6ms",
    "2026-10-18T21:30:00Z  INFO migo_core::runtime::host: [Host 1] evaluate_module('player-demo', 'game.js'): 88ms",
]
engine_errors = [
    "2026-10-18T21:07:51Z ERROR migo_core::runtime::host: [Host 1] handle_command failed: "
    "e=[ModuleLoadError] module evaluation failed (ReferenceError: notDefinedAnywhere is not defined",
    "2026-10-18T21:07:51Z ERROR runtime_v8::console: Unhandled Promise Rejection: "
    "ReferenceError: notDefinedAnywhere is not defined",
    "2026-10-18T20:37:18Z ERROR migo_core::runtime::thread: [Host 1] event loop error: code=JsException, "
    "msg=run_event_loop, detail=Some(\"TypeError: thrown from a timer\\n    at file:///game.js:2:32\")",
    "2026-10-18T20:47:48Z ERROR migo_core::runtime::host: [Host 1] handle_command failed: e=[ModuleLoadError] "
    "load main es module (failed to read module: ByteLimitExceeded { requested: 104857600, available: 0 })",
    "2026-10-18T20:00:00Z ERROR migo_core::runtime::host: [Host 1] prelude script 'wx-adapter.js' failed: boom",
    "2026-10-18T21:07:51Z ERROR runtime_v8::console: Unhandled Promise Rejection: "
    "ReferenceError: notDefinedAnywhere is not defined",
]
# A Cocos Creator 2 mini-game build, 2026-10-19: it loads its engine through the platform's plugin service, and
# asked for network status, which the Linux player has no service for.
engine_refusals = [
    "2026-10-18T22:07:09Z ERROR runtime_v8::console: Failed to start network monitoring: "
    "Error: answeredVerified:fail not supported",
    "2026-10-18T22:07:09Z ERROR runtime_v8::console: loadSubpackage success callback error: "
    "ReferenceError: requirePlugin is not defined",
    "2026-10-18T22:07:10Z ERROR runtime_v8::console: again: answeredVerified:fail not supported",
]
lines = log + engine_ready + engine_errors + engine_refusals
events = cr.events(lines)
check("every marker line with an object is an event, wherever the marker sits, and nothing else is",
      len(events) == 12, f"{len(events)} events")
errors = cr.content_errors(lines)
labels = [(e["label"], e["fatal"]) for e in errors]
check("each engine error line is labelled once, by its most specific shape", labels == [
    ("the entry threw", True),
    ("a rejection nobody handled", False),
    ("an exception escaped a callback, and the game stopped", True),
    ("the entry did not load", True),
    ("a prelude failed", True),
    ("a script error", False),
], labels)

check("ready is the engine's measure from the first launch in the log", cr.ready_ms(lines) == 191.6, cr.ready_ms(lines))
check("and a log without it has none", cr.ready_ms(log + engine_errors) is None)
report = cr.assess(lines, "android", contracts)
calls = {(c["surface"], c["api"]): c for c in report["calls"]}
check("a wx name for migo's API is judged by migo's name",
      calls[("wx", "updatePlatformApp")].get("platform_api") == "updateHostApp"
      and calls[("wx", "updatePlatformApp")]["verdict"] == "host-owned", calls.get(("wx", "updatePlatformApp")))
check("calls come worst first", [c["verdict"] for c in report["calls"]] == ["failing", "host-owned", "verified", "outside"],
      [c["verdict"] for c in report["calls"]])
check("absent names say whether they are platform APIs",
      report["absent"] == [{"surface": "wx", "api": "notOffered", "platform_api": True, "answered_by": "unsupported"},
                           {"surface": "wx", "api": "cloud", "platform_api": False}], report["absent"])
check("the GL error is named", report["webgl"]["errors"] == [{"context": "webgl", "code": 1282, "name": "INVALID_OPERATION"}])
check("timing is the run's", report["timing"] == {"ready_ms": 191.6, "first_frame_ms": 190.7})
check("a surface the recorder left alone is said to be", report["unrecorded"] == [{"surface": "web", "reason": "accessor"}]
      and "`web` surface was not recorded" in "\n".join(cr.markdown(report)))
check("blocking is the failing call and the fatal errors",
      [b.get("api") or b["label"] for b in report["blocking"]]
      == ["breaks", "the entry threw", "an exception escaped a callback, and the game stopped",
          "the entry did not load", "a prelude failed"], report["blocking"])
text = "\n".join(cr.markdown(report))
check("the Markdown leads with the blocking count", "**5 blocking item(s).**" in text)
check("a call the host refused is judged for the report's platform, once",
      [(r["api"], r["verdict"]) for r in report["refused"]] == [("answeredVerified", "verified")], report["refused"])
check("a global nothing defined is named, platform API or not",
      report["undefined"] == [{"name": "notDefinedAnywhere", "platform_api": False},
                              {"name": "requirePlugin", "platform_api": False}], report["undefined"])
frameless = cr.assess([l for l in log if "first-frame" not in l], "android", contracts)
check("a recorded run with no animation frame says so, and does not claim the game never drew",
      [b.get("label") for b in frameless["blocking"]] == [None] and len(frameless["notes"]) == 1
      and "requestAnimationFrame" in "\n".join(cr.markdown(frameless)), (frameless["blocking"], frameless["notes"]))
dark = cr.assess([l for l in log if "first-frame" not in l]
                 + ["2026-10-18T22:07:47Z ERROR migo_player: player failed: nothing was presented during the run"],
                 "android", contracts)
check("the player's own word that nothing was presented is what blocks on it",
      [b.get("label") for b in dark["blocking"]] == [None, "nothing was presented in this run: the game never drew"]
      and dark["notes"] == [], (dark["blocking"], dark["notes"]))

unrecorded = cr.assess(engine_errors, "android", contracts)
check("a log without the recorder's start is not recorded", unrecorded["recorded"] is False)
check("and its report says nothing else", "**Not recorded.**" in "\n".join(cr.markdown(unrecorded))
      and "## Verdict" not in "\n".join(cr.markdown(unrecorded)))
check("and says nothing of frames it was never asked about", unrecorded["notes"] == []
      and all("presented" not in b.get("label", "") for b in unrecorded["blocking"]))

real = cr.load_contracts("android")
verdicts = {name: cr.judge(name, "android", real)["verdict"] for name in real["matrix"]}
check("every API of the real matrix gets a verdict on android",
      all(v in cr.VERDICTS and v != "outside" for v in verdicts.values()))
check("and every one the real decisions leave unsupported reads unsupported, with its reason",
      all(verdicts[n] == "unsupported" and cr.judge(n, "android", real).get("reason")
          for n, d in real["decisions"]["unsupported"].items()
          if n in real["matrix"] and (d["platforms"] == "*" or "android" in d["platforms"])))

print("PASS" if failures == 0 else f"FAIL: {failures} check(s) failed")
sys.exit(1 if failures else 0)
PY
