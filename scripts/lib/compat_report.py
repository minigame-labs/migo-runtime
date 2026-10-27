"""What a game reached for in one run, and what this build's contracts say of each thing it reached for.

The compatibility recorder (tools/compat-recorder/recorder.js) writes one marker-prefixed JSON object per
line into the log; a log is anything that carries those lines -- logcat, the headless player's tracing --
so the marker is looked for anywhere in a line. Errors and the time to the end of the entry's evaluation
are not the recorder's: the engine logs them itself, and content_errors() and ready_ms() read them from
the same log. Both are INFO-level or above, which a host that injects the recorder must log anyway.

Each API the run called or looked for is judged on one platform by two contracts:
  contracts/runtime/api-platform-matrix.json   who answers it there -- the engine, the platform's own host
                                               layer, the host app, or nothing yet -- and what it needs
  contracts/runtime/api-conformance-<p>.json    whether the conformance suite exercised it there and every
                                               check passed

  python3 scripts/lib/compat_report.py LOG [--platform android] [--json FILE] [--out FILE] [--note TEXT]
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
MARK = "__MIGO_COMPAT__"
ANSI = re.compile(r"\x1b\[[0-9;]*m")
SCHEMA = 1

GL_ERRORS = {
    1280: "INVALID_ENUM",
    1281: "INVALID_VALUE",
    1282: "INVALID_OPERATION",
    1285: "OUT_OF_MEMORY",
    1286: "INVALID_FRAMEBUFFER_OPERATION",
    37442: "CONTEXT_LOST_WEBGL",
}

# A called API's verdict, worst first. "host-owned" is not a gap: what is left of it to answer is the host
# app's by nature (its ads, payments, accounts, consent), and no platform run can verify that for it.
VERDICTS = {
    "unsupported": "deliberately not offered on this platform",
    "missing": "no way to make it work on this platform yet",
    "failing": "a conformance check of it fails on this platform",
    "host-no-default": "the host app must provide it; the platform has no default yet",
    "unverified": "answered here, but no conformance run has verified it",
    "host-owned": "the host app's own integration answers it (its ads, payments, accounts, consent), the platform the rest",
    "verified": "answered here, and verified by the conformance suite",
    "beyond-reference": "the reference does not offer it on this kind of client (a phones-only call on a desktop, or a "
                        "desktop-only one on a phone), so nothing here is owed to compatibility",
    "outside": "not an API of the mini-game platform (an engine global, or migo's own)",
}
BLOCKING = ("unsupported", "missing", "failing")

# What the engine logs when content fails, most specific first: a line is labelled once, by the first match.
# Fatal ones end the launch or the game; the rest are the game's own failures, which it may survive.
CONTENT_ERRORS = [
    (r"module evaluation failed.*", "the entry threw", True),
    (r"load main es module.*", "the entry did not load", True),
    (r"prelude script '[^']*' failed.*", "a prelude failed", True),
    (r"event loop error: .*", "an exception escaped a callback, and the game stopped", True),
    (r"\bFATAL EXCEPTION\b.*", "the host crashed", True),
    (r"Unhandled Promise Rejection: .*", "a rejection nobody handled", False),
    (r"\bUncaught\b.*", "an uncaught exception", False),
    (r"\bmigo_[a-z_]*error\b.*", "a C ABI error", False),
    (r"\b(ReferenceError|TypeError|SyntaxError)\b.*", "a script error", False),
]


def events(lines) -> list[dict]:
    """The recorder's events, in the order the log holds them."""
    found = []
    for raw in lines:
        line = ANSI.sub("", raw.rstrip("\n"))
        at = line.find(MARK)
        if at < 0:
            continue
        try:
            event = json.loads(line[at + len(MARK):].strip())
        except json.JSONDecodeError:
            continue
        if isinstance(event, dict) and isinstance(event.get("kind"), str):
            found.append(event)
    return found


# The host's own line when the entry has been evaluated, top-level awaits included: from the launch, so it
# counts the preludes and the module's loading too.
READY = re.compile(r"evaluate_module\('[^']*', '[^']*'\): ([0-9]+(?:\.[0-9]+)?)ms")


def ready_ms(lines) -> float | None:
    """Milliseconds from the launch to the end of the entry's evaluation, from the first launch the log holds."""
    for raw in lines:
        match = READY.search(ANSI.sub("", raw))
        if match:
            return float(match.group(1))
    return None


# The engine's answer to a call it does not support where it runs (`<api>:fail not supported`). On the Linux
# player that is every call needing a device service, so each is judged for the platform the report is for:
# what Linux refused, Android may answer.
REFUSED = re.compile(r"\b([A-Za-z_$][\w$]*):fail not supported\b")
# The headless player's own verdict when no frame reached its window: the evidence that a game never drew, which the
# recorder cannot give -- it sees requestAnimationFrame callbacks, and content can draw without one.
NOTHING_PRESENTED = re.compile(r"nothing was presented during the run")
# The headless player's own verdict when the engine reported that the launch failed. It ends the run there, so a game
# that did not start says so instead of running out the present ceiling to "nothing was presented". Its reason is the
# engine's, which CONTENT_ERRORS already labels from the engine's own line when the content caused it; read as content
# too, one failure would be listed twice.
DID_NOT_START = re.compile(r"the game did not start: (.*)")
# A global the content used and no layer defined: not a name on the adapter's namespace or `migo` (the recorder
# sees those), so
# only the engine's error says it.
UNDEFINED = re.compile(r"ReferenceError: ([A-Za-z_$][\w$]*) is not defined")


def _first_matches(pattern, lines) -> list[str]:
    found = []
    for raw in lines:
        for match in pattern.finditer(ANSI.sub("", raw)):
            if MARK not in raw and match.group(1) not in found:
                found.append(match.group(1))
    return found


def refused_calls(lines) -> list[str]:
    """Each API the host answered `<api>:fail not supported`, once, in the order the log holds them."""
    return _first_matches(REFUSED, lines)


def content_errors(lines) -> list[dict]:
    """Each distinct line the engine logged about content failing, labelled by the first pattern it matches."""
    found, seen = [], set()
    for raw in lines:
        line = ANSI.sub("", raw.rstrip("\n"))
        if MARK in line or DID_NOT_START.search(line):
            continue
        for pattern, label, fatal in CONTENT_ERRORS:
            match = re.search(pattern, line)
            if match:
                text = match.group(0)[:400]
                if text not in seen:
                    seen.add(text)
                    found.append({"label": label, "fatal": fatal, "line": text})
                break
    return found


def load_contracts(platform: str, root: pathlib.Path = ROOT) -> dict:
    runtime = root / "contracts/runtime"
    return {
        "matrix": json.loads((runtime / "api-platform-matrix.json").read_text())["apis"],
        "decisions": json.loads((runtime / "api-platform-decisions.json").read_text()),
        "conformance": json.loads((runtime / f"api-conformance-{platform}.json").read_text()),
    }


def judge(name: str, platform: str, contracts: dict) -> dict:
    """What the contracts say of the platform API `name` on `platform`.

    Readiness is the matrix's own: its generator holds a host cell ready only where every need is the host
    app's by nature or has a default, and an engine cell only where the conformance run verified it. This
    reads that answer rather than deriving it again.
    """
    cell = contracts["matrix"].get(name)
    if cell is None:
        return {"verdict": "outside"}
    wired = cell.get(platform)
    ready = platform not in cell.get("not_ready", [])
    record = contracts["conformance"]["apis"].get(name, {})
    status = record.get("status", "unmeasured")
    judged = {"category": cell.get("category"), "answered_by": wired, "needs": cell.get("needs", []),
              "conformance": status}
    if record.get("failing"):
        judged["failing"] = record["failing"]
    if platform in cell.get("outside", []):
        judged["verdict"] = "beyond-reference"
    elif wired == "unsupported":
        judged["verdict"] = "unsupported"
        judged["reason"] = contracts["decisions"]["unsupported"].get(name, {}).get("reason")
    elif wired == "missing":
        judged["verdict"] = "missing"
    elif status == "failing":
        judged["verdict"] = "failing"
    elif wired == "host":
        judged["verdict"] = "host-owned" if ready else "host-no-default"
    else:
        judged["verdict"] = "verified" if ready else "unverified"
    return judged


def assess(lines, platform: str, contracts: dict) -> dict:
    """One run's report, from its log."""
    kinds = {}
    for event in events(lines):
        kinds.setdefault(event["kind"], []).append(event)
    calls = []
    for event in kinds.get("call", []):
        name = event.get("migo") or event["api"]
        entry = {"surface": event["surface"], "api": event["api"]}
        if name != event["api"]:
            entry["platform_api"] = name
        entry.update(judge(name, platform, contracts))
        calls.append(entry)
    order = list(VERDICTS)
    calls.sort(key=lambda c: (order.index(c["verdict"]), c["surface"], c["api"]))
    absent = []
    for event in kinds.get("absent", []):
        cell = contracts["matrix"].get(event["api"])
        entry = {"surface": event["surface"], "api": event["api"], "platform_api": cell is not None}
        if cell is not None:
            entry["answered_by"] = cell.get(platform)
        absent.append(entry)
    refused = [dict({"api": name}, **judge(name, platform, contracts)) for name in refused_calls(lines)]
    undefined = [{"name": name, "platform_api": name in contracts["matrix"]} for name in _first_matches(UNDEFINED, lines)]
    errors = content_errors(lines)
    first = kinds.get("first-frame", [])
    recorded = bool(kinds.get("start"))
    blocking = [c for c in calls if c["verdict"] in BLOCKING] + [e for e in errors if e["fatal"]]
    notes = []
    not_started = _first_matches(DID_NOT_START, lines)[:1]
    for reason in not_started:
        blocking.append({"label": "the game did not start", "fatal": True, "line": reason[:400]})
    if any(NOTHING_PRESENTED.search(ANSI.sub("", line)) for line in lines):
        blocking.append({"label": "nothing was presented in this run: the game never drew", "fatal": True, "line": ""})
    elif recorded and not first and not not_started:
        notes.append("No animation frame ran in this run. The recorder sees requestAnimationFrame callbacks, so content "
                     "that draws without them reads the same; whether anything reached the screen is the frame's to say.")
    return {
        "schema": SCHEMA,
        "platform": platform,
        "conformance_measured": contracts["conformance"].get("measured"),
        "recorded": recorded,
        "calls": calls,
        "absent": absent,
        "refused": refused,
        "undefined": undefined,
        "installed": [{"surface": e["surface"], "api": e["api"]} for e in kinds.get("installed", [])],
        "webgl": {
            "extensions": [{k: e.get(k) for k in ("context", "name", "supported")}
                           for e in kinds.get("gl-extension", [])],
            "errors": [{"context": e.get("context"), "code": e.get("code"),
                        "name": GL_ERRORS.get(e.get("code"), "unknown")} for e in kinds.get("gl-error", [])],
        },
        "timing": {"ready_ms": ready_ms(lines), "first_frame_ms": first[0]["ms"] if first else None},
        "unrecorded": [{"surface": e["surface"], "reason": e.get("reason")} for e in kinds.get("unrecorded", [])],
        "errors": errors,
        "summary": {verdict: sum(1 for c in calls if c["verdict"] == verdict) for verdict in VERDICTS},
        "blocking": blocking,
        "notes": notes,
    }


def markdown(report: dict, notes: tuple[str, ...] | list[str] = ()) -> list[str]:
    out = [f"# What the game reached for, judged for {report['platform']}", ""]
    for note in notes:
        out += [f"> {note}", ""]
    if not report["recorded"]:
        return out + ["**Not recorded.** The log holds no line from the compatibility recorder, so it was not "
                      "injected or the game never started. Nothing below would mean anything.", ""]
    # One entry per run the platform's record merges (a host each).
    runs = report.get("conformance_measured") or []
    measured_by = ", ".join(f"{run.get('runner')} on {run.get('date')}" for run in runs) or "no run"
    out += [f"Statuses are this build's contracts for {report['platform']}: who answers each API there, and the "
            f"conformance runs it records ({measured_by}). A call is recorded once, the first time; "
            "what a run did not reach is not here -- this is one run, not every path through the game.", ""]
    for gap in report["unrecorded"]:
        out += [f"> The `{gap['surface']}` surface was not recorded: the global is {'frozen' if gap['reason'] == 'frozen' else 'an accessor'}, "
                "and standing in for it would change it. Nothing it was asked for is below.", ""]
    blocking = report["blocking"]
    out += ["## Verdict", "",
            f"**{len(blocking)} blocking item(s).**" if blocking else "**Nothing blocking was reached in this run.**",
            ""]
    for item in blocking:
        if "verdict" in item:
            out.append(f"- `{item['surface']}.{item['api']}` -- {VERDICTS[item['verdict']]}"
                       + (f": {item['reason']}" if item.get("reason") else ""))
        else:
            out.append(f"- {item['label']}" + (f": `{item['line']}`" if item["line"] else ""))
    for note in report["notes"]:
        out += ["", f"> {note}"]
    out += ["", "## APIs called", "", "| verdict | count |", "|---|---|"]
    out += [f"| {VERDICTS[v]} | {n} |" for v, n in report["summary"].items() if n]
    out += [""]
    if report["calls"]:
        out += ["| API | verdict | answered by | needs | conformance |", "|---|---|---|---|---|"]
        for call in report["calls"]:
            name = f"`{call['surface']}.{call['api']}`" + (f" (`{call['platform_api']}`)" if "platform_api" in call else "")
            out.append(f"| {name} | {call['verdict']} | {call.get('answered_by', '')} | "
                       f"{', '.join(call.get('needs', []))} | {call.get('conformance', '')} |")
        out.append("")
    out += ["## Names it looked for and did not find", ""]
    if report["absent"]:
        out += ["Feature detection, or a call about to throw. A platform API here is one this build does not "
                "publish; whether the game copes is the game's fallback, which a run that did not crash suggests.", ""]
        for entry in report["absent"]:
            kind = f"a platform API ({entry.get('answered_by')})" if entry["platform_api"] else "not a platform API"
            out.append(f"- `{entry['surface']}.{entry['api']}` -- {kind}")
    else:
        out.append("None.")
    out += ["", "## Calls answered \"not supported\"", ""]
    if report["refused"]:
        out += ["The host this ran on refused these. The verdict is the platform's the report is for: a call the Linux "
                "player refuses for want of a device service may be answered there.", ""]
        out += [f"- `{r['api']}` -- {r['verdict']}" + (f" ({r.get('answered_by')})" if r.get("answered_by") else "")
                for r in report["refused"]]
    else:
        out.append("None.")
    out += ["", "## Globals it needed that are not defined", ""]
    out += [f"- `{u['name']}`" + (" -- a platform API" if u["platform_api"] else
            " -- not an API of the mini-game platform: a name only the content's original host defines")
            for u in report["undefined"]] or ["None."]
    out += ["", "## Names it installed itself", ""]
    out += [f"- `{e['surface']}.{e['api']}`" for e in report["installed"]] or ["None."]
    out += ["", "## WebGL", ""]
    gl = report["webgl"]
    out += [f"- {e['context']} `{e['name']}`: {'supported' if e['supported'] else 'not supported'}"
            for e in gl["extensions"]] or ["No extension was asked for."]
    out += [f"- {e['context']} error {e['code']} ({e['name']})" for e in gl["errors"]]
    out += ["", "## Content errors", ""]
    out += [f"- **{e['label']}**{' (fatal)' if e['fatal'] else ''}: `{e['line']}`" for e in report["errors"]] \
        or ["None matched the shapes the engine logs. Content can still fail quietly."]
    timing = report["timing"]
    out += ["", "## Timing", "",
            f"- from the launch to the end of the entry's evaluation (the engine's measure): {timing['ready_ms']} ms"
            if timing["ready_ms"] is not None else "- the entry's evaluation never ended",
            f"- from the recorder's start to the first animation frame: {timing['first_frame_ms']} ms"
            if timing["first_frame_ms"] is not None else "- no animation frame ran", ""]
    return out


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description="A compatibility report from one run's log.")
    parser.add_argument("log", type=pathlib.Path)
    parser.add_argument("--platform", default="android")
    parser.add_argument("--json", type=pathlib.Path)
    parser.add_argument("--out", type=pathlib.Path)
    parser.add_argument("--note", action="append", default=[])
    args = parser.parse_args(argv)
    lines = args.log.read_text(encoding="utf-8", errors="replace").splitlines()
    report = assess(lines, args.platform, load_contracts(args.platform))
    text = "\n".join(markdown(report, args.note)) + "\n"
    if args.json:
        args.json.write_text(json.dumps(report, indent=2) + "\n")
    if args.out:
        args.out.write_text(text)
    else:
        sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
