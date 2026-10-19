"""The device run's report, from what scripts/prescreen-run.sh captured into its work directory.

prescreen-run.sh drives the device -- deploys, launches, waits, screenshots, pulls the log -- and this reads
what it left: the two PNGs, the focused window at each capture, the log and its scope. Split out of the script
so the reading can be tested on a capture made by hand, where a device run cannot be: every verdict below has
been wrong at least once, and a verdict nobody can exercise without a phone is one nobody exercises.

  python3 scripts/lib/prescreen_run_report.py BUNDLE WORK PKG ACTIVITY MODEL SDK SECS ALIVE REPORT_DIR
"""

from __future__ import annotations

import pathlib
import re
import sys
import zlib

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import compat_report  # noqa: E402  -- one table of the engine's error lines, shared with the Linux report


def png_stats(path: pathlib.Path):
    """Distinct colours and a cheap content hash, without an image library.

    A prescreen has to run on a customer's machine; requiring Pillow to answer
    "is anything on screen" would be a dependency for a yes/no. This decodes the
    IDAT stream and samples it, which needs only zlib from the standard library.
    """
    if not path.exists() or path.stat().st_size == 0:
        return None
    raw = path.read_bytes()
    if raw[:8] != b"\x89PNG\r\n\x1a\n":
        return None
    pos, idat = 8, bytearray()
    width = height = 0
    while pos + 8 <= len(raw):
        length = int.from_bytes(raw[pos : pos + 4], "big")
        kind = raw[pos + 4 : pos + 8]
        body = raw[pos + 8 : pos + 8 + length]
        if kind == b"IHDR":
            width = int.from_bytes(body[0:4], "big")
            height = int.from_bytes(body[4:8], "big")
        elif kind == b"IDAT":
            idat += body
        elif kind == b"IEND":
            break
        pos += 12 + length
    if not idat or not width:
        return None
    try:
        data = zlib.decompress(bytes(idat))
    except zlib.error:
        return None
    # Sample rather than decode every scanline: filters make exact reconstruction
    # costly, and for "one flat colour or not" a sample is sufficient and honest
    # about being one.
    step = max(1, len(data) // 60000)
    sample = data[::step]
    colours = len(set(sample[i : i + 3] for i in range(0, len(sample) - 3, 3)))
    return {
        "size": f"{width}x{height}",
        "colours": colours,
        "digest": zlib.crc32(sample),
        "bytes": path.stat().st_size,
    }


def read_focus(work: pathlib.Path, name: str) -> str:
    path = work / name
    if not path.exists():
        return ""
    text = path.read_text(encoding="utf-8", errors="replace").strip()
    match = re.search(r"(?:mCurrentFocus=Window|ResumedActivity: ActivityRecord)\{\S+ \S+ ([^} ]+)", text)
    return match.group(1) if match else text


def report(bundle: str, work: pathlib.Path, pkg: str, activity: str, model: str, sdk: str, secs: str, alive: str,
           report_dir: str) -> str:
    first = png_stats(work / "first.png")
    second = png_stats(work / "second.png")
    focus1, focus2 = read_focus(work, "focus1.txt"), read_focus(work, "focus2.txt")
    # The activity the caller named, without the package prefix, as dumpsys spells it.
    wanted = activity.split("/", 1)[1] if "/" in activity else activity
    wanted_full = wanted if wanted.startswith(".") is False else pkg + wanted
    focus_known = bool(focus1)
    focus_is_wanted = focus_known and (wanted_full in focus1 or wanted in focus1)
    focus_in_pkg = focus_known and pkg in focus1

    log = ""
    log_path = work / "logcat.txt"
    if log_path.exists():
        log = log_path.read_text(encoding="utf-8", errors="replace")

    # Did a session actually run? The Java SDK logs its lifecycle at default level,
    # with no configuration, on any host that uses it -- which makes it the one
    # host-agnostic signal that the engine was handed this content at all.
    #
    # Pixels cannot supply this. An earlier version of this tool was pointed at a
    # host's menu screen, saw a colourful surface belonging to the host's own UI, and
    # reported "Runs" for a bundle that was never loaded. A verdict that can be
    # reached without the engine ever starting is not a verdict about the engine.
    session_running = bool(re.search(r"MigoSession.*state:.*->\s*RUNNING", log))
    session_seen = bool(re.search(r"\bMigoSession\b", log))

    # The engine's own error lines, read by the table the Linux report reads them by: this one used to keep its
    # own, and it had no pattern for an exception escaping a callback ("event loop error"), which ends the game
    # -- so a game that died in its first timer after painting a frame read as "Runs".
    lines = log.splitlines()
    found = [(error["label"], error["line"][:200]) for error in compat_report.content_errors(lines)]
    fatal = {error["line"][:200] for error in compat_report.content_errors(lines) if error["fatal"]}
    refused = compat_report.refused_calls(lines)

    painted = bool(first and first["colours"] > 8)
    moving = bool(first and second and first["digest"] != second["digest"])
    crashed = any(label == "the host crashed" for label, _ in found)
    running = bool(alive)

    out: list[str] = []
    out.append(f"# Prescreen (run): `{pathlib.Path(bundle).name}`")
    out.append("")
    out.append(f"- host: `{pkg}` — `{activity}`")
    out.append(f"- device: {model or 'unknown'} (API {sdk or '?'})")
    out.append(f"- observed for {secs}s")
    out.append(f"- window in front at capture: `{focus1 or 'unknown'}`")
    scope_path = work / "log-scope.txt"
    log_scope = scope_path.read_text(encoding="utf-8").strip() if scope_path.exists() else "unknown"
    out.append(f"- log scope: {log_scope}")
    out.append(
        f"- engine session reached RUNNING: {'yes' if session_running else 'no'}"
        + ("" if session_seen else " (no `MigoSession` line at all)")
    )
    if focus2 and focus2 != focus1:
        out.append(f"- and at the second capture: `{focus2}`")
    out.append("")
    if not focus_known:
        out.append(
            "> **Could not read which window was in front.** The sections below stand "
            "on their own, but nothing here confirms the host owned the screen."
        )
        out.append("")
    elif not focus_in_pkg:
        out.append(
            "> **The host was not the window in front.** Everything below describes "
            "whatever was, so none of it is evidence about this bundle."
        )
        out.append("")
    elif not focus_is_wanted:
        out.append(
            f"> The window in front is `{focus1}`, not the activity that was asked for "
            f"(`{wanted}`) — normal when a host forwards its launch entry to its own "
            "game activity. Whether content is actually on screen is settled below by "
            "the session signal, not by this name."
        )
        out.append("")

    # --- 1. did it paint ---
    out.append("## 1. Did it put a frame on screen")
    out.append("")
    if first is None:
        out.append("**No screenshot could be captured.** Nothing can be said about rendering.")
    else:
        out.append(f"| | frame 1 (t≈{int(secs)//2}s) | frame 2 (t≈{secs}s) |")
        out.append("|---|---|---|")
        out.append(f"| distinct colours sampled | {first['colours']} | {second['colours'] if second else 'n/a'} |")
        out.append(f"| surface | {first['size']} | {second['size'] if second else 'n/a'} |")
        out.append("")
        if painted and moving:
            out.append("**Painted, and still changing between the two captures.**")
        elif painted:
            out.append(
                "**Painted, but the two captures are identical.** Either the scene is "
                "static by then, or it stopped. Look at the PNGs before reading this "
                "as a stall."
            )
        else:
            out.append(
                "**Effectively one flat colour — nothing recognisable was drawn.** "
                "A game that legitimately opens on a solid fill reads the same way, so "
                "check the PNG before calling this a failure."
            )
        out.append("")
        out.append(f"Evidence: `{report_dir}/prescreen-frame-1.png`, `{report_dir}/prescreen-frame-2.png`")
    out.append("")

    # --- 2. what the log said ---
    out.append("## 2. Content-side errors")
    out.append("")
    if not log:
        out.append("No log was captured, so this section proves nothing either way.")
    elif not found:
        out.append(
            "None matched. That is not the same as none happening — this greps for the "
            "shapes this runtime emits, and content can fail quietly."
        )
    else:
        out.append(f"{len(found)} distinct line(s):")
        out.append("")
        for label, line in found[:40]:
            out.append(f"- **{label}** — `{line}`")
        if len(found) > 40:
            out.append(f"- ... and {len(found) - 40} more")
    out.append("")
    if refused:
        out.append(
            "Calls this host answered \"not supported\": "
            + ", ".join(f"`{name}`" for name in refused)
            + ". On a device that is the host's or the platform's answer, not a missing service of the machine."
        )
        out.append("")

    # --- 3. verdict ---
    out.append("## 3. Verdict")
    out.append("")
    blockers = [f"{label}: `{line}`" for label, line in found if line in fatal]
    if focus_known and not focus_in_pkg:
        verdict = "**Nothing observed**"
        why = (
            f"`{focus1}` held the foreground, not the host, so the screen and the log "
            "describe something else. Check that the activity exists and starts."
        )
    elif crashed or not running:
        verdict = "**Not runnable as-is**"
        why = "the host process was gone before the observation window ended."
    elif blockers:
        verdict = "**Blocked**"
        why = "it started, and content-side failures were logged."
    elif not session_running:
        verdict = "**No engine session ran**"
        if session_seen:
            why = (
                "the host was on screen and the SDK logged session activity, but no "
                "session reached RUNNING. The bundle was not started."
            )
        else:
            why = (
                "nothing on screen came from a Migo session — there is no `MigoSession` "
                "line in the log at all. Either the activity named is not the one that "
                "hosts the game (a menu screen looks just as alive in a screenshot), or "
                "this host embeds the engine through the C ABI without the Java SDK, "
                "which logs no lifecycle. Either way, the pixels below are not evidence "
                "about this bundle."
            )
    elif painted and moving:
        verdict = "**Runs**"
        why = f"a session ran, and it painted a changing scene for {secs}s with no logged content failure."
    elif painted:
        verdict = "**Runs, with a caveat**"
        why = "a session ran and painted, but the scene stopped changing. Check the frames."
    else:
        verdict = "**Inconclusive**"
        why = "the process stayed up and logged nothing, but nothing recognisable was drawn."
    out.append(f"{verdict} — {why}")
    out.append("")
    if blockers:
        out.append("Blockers:")
        out.append("")
        for item in blockers[:20]:
            out.append(f"- {item}")
        out.append("")

    out.append("## What this is not")
    out.append("")
    out.append(
        f"- **Not a certification.** {secs} seconds, one device, one launch. It says "
        "nothing about level two, about a cold cache, or about a device you do not own."
    )
    out.append(
        "- **Not the whole prescreen.** Run `scripts/prescreen-game.sh` as well: this "
        "half sees what broke, that half sees which APIs the bundle needs and whether "
        "this build has them. Neither answers the other's question."
    )
    out.append(
        "- **Not an instrumented measurement.** It watches the screen and the log, "
        "because the bundle belongs to someone else and was not built to be watched."
    )

    # What the game reached for, when the host injected the compatibility recorder (and logs at INFO, which a
    # release engine does not by default). Without it there is nothing to read, and nothing is said.
    if any(event.get("kind") == "start" for event in compat_report.events(lines)):
        reach = compat_report.assess(lines, "android", compat_report.load_contracts("android"))
        out.append("## 4. What it reached for")
        out.append("")
        out.extend("#" + line if line.startswith("#") else line for line in compat_report.markdown(reach)[1:])

    return "\n".join(out)


def main(argv: list[str]) -> int:
    bundle, work, pkg, activity, model, sdk, secs, alive, report_dir = argv[:9]
    print(report(bundle, pathlib.Path(work), pkg, activity, model, sdk, secs, alive, report_dir))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))