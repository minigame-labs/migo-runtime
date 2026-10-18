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


def content_errors(lines) -> list[dict]:
    """Each distinct line the engine logged about content failing, labelled by the first pattern it matches."""
    found, seen = [], set()
    for raw in lines:
        line = ANSI.sub("", raw.rstrip("\n"))
        if MARK in line:
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
    if wired == "unsupported":
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


def assess(recorded: list[dict], errors: list[dict], platform: str, contracts: dict,
           ready: float | None = None) -> dict:
    kinds = {}
    for event in recorded:
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
    first = kinds.get("first-frame", [])
    summary = {verdict: sum(1 for c in calls if c["verdict"] == verdict) for verdict in VERDICTS}
    return {
        "schema": SCHEMA,
        "platform": platform,
        "conformance_measured": contracts["conformance"].get("measured"),
        "recorded": bool(kinds.get("start")),
        "calls": calls,
        "absent": absent,
        "installed": [{"surface": e["surface"], "api": e["api"]} for e in kinds.get("installed", [])],
        "webgl": {
            "extensions": [{k: e.get(k) for k in ("context", "name", "supported")}
                           for e in kinds.get("gl-extension", [])],
            "errors": [{"context": e.get("context"), "code": e.get("code"),
                        "name": GL_ERRORS.get(e.get("code"), "unknown")} for e in kinds.get("gl-error", [])],
        },
        "timing": {"ready_ms": ready, "first_frame_ms": first[0]["ms"] if first else None},
        "unrecorded": [{"surface": e["surface"], "reason": e.get("reason")} for e in kinds.get("unrecorded", [])],
        "errors": errors,
        "summary": summary,
        "blocking": [c for c in calls if c["verdict"] in BLOCKING] + [e for e in errors if e["fatal"]],
    }


def markdown(report: dict, notes: tuple[str, ...] | list[str] = ()) -> list[str]:
    out = [f"# What the game reached for, judged for {report['platform']}", ""]
    for note in notes:
        out += [f"> {note}", ""]
    if not report["recorded"]:
        return out + ["**Not recorded.** The log holds no line from the compatibility recorder, so it was not "
                      "injected or the game never started. Nothing below would mean anything.", ""]
    measured = report.get("conformance_measured") or {}
    out += [f"Statuses are this build's contracts for {report['platform']}: who answers each API there, and the "
            f"conformance run of {measured.get('date', 'an unknown date')}. A call is recorded once, the first time; "
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
            out.append(f"- {item['label']}: `{item['line']}`")
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
    report = assess(events(lines), content_errors(lines), args.platform, load_contracts(args.platform),
                    ready_ms(lines))
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
