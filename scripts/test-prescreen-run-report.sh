#!/usr/bin/env bash
# The device run's report says what the capture shows, read by the engine's own error lines.
#
# scripts/prescreen-run.sh needs a phone, so its verdicts were exercised only when someone had one --
# and its reading of the log kept a pattern table of its own, apart from the one the Linux report uses.
# That table had no pattern for the engine's fatal "event loop error" (an exception escaping a
# callback ends the game) unless the exception happened to be a TypeError or a ReferenceError: a
# game that painted its first frames and then died in a timer read as "Runs". The reading now lives
# in scripts/lib/prescreen_run_report.py and uses compat_report's table, and this holds each verdict
# to a capture made here: the PNGs, the focused window, the log in the device's own format.
#
# It also holds the run's preludes (--adapter, --record) to the host's word: a host that ignores the
# request runs without them, so the report calls one injected only when the host's log names its
# digest, and says when it does not.
#
# Host-only: python3, no device.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

python3 - "$ROOT" <<'PY'
import pathlib
import struct
import subprocess
import sys
import tempfile
import zlib

root = pathlib.Path(sys.argv[1])
reporter = root / "scripts/lib/prescreen_run_report.py"
PKG = "com.example.host"
failures = 0


def png(path: pathlib.Path, seed: int, flat: bool = False) -> None:
    width, height = 64, 64
    rows = []
    for y in range(height):
        row = bytearray(b"\x00")
        for x in range(width):
            row += bytes((40, 40, 40)) if flat else bytes(((x * 7 + seed) % 256, (y * 13 + seed) % 256, (x + y) % 256))
        rows.append(bytes(row))

    def chunk(kind: bytes, data: bytes) -> bytes:
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    ihdr = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(b"".join(rows)))
                     + chunk(b"IEND", b""))


RUNNING = "10-19 05:00:01.000  4321  4321 D MigoSession: Session 1 state: STARTING -> RUNNING"


def run(label: str, log_lines: list[str], *, painted: bool = True, moving: bool = True, alive: str = "4321",
        front: str = f"{PKG}/{PKG}.GameActivity", preludes: tuple = (), host_pid: str = "4321") -> str:
    with tempfile.TemporaryDirectory() as work_dir:
        work = pathlib.Path(work_dir)
        png(work / "first.png", 1, flat=not painted)
        png(work / "second.png", 2 if moving else 1, flat=not painted)
        for name in ("focus1.txt", "focus2.txt"):
            (work / name).write_text(f"  mCurrentFocus=Window{{a1b2c3 u0 {front}}}\n")
        (work / "logcat.txt").write_text("\n".join(log_lines) + "\n")
        (work / "log-scope.txt").write_text("pid 4321\n")
        (work / "host-pid.txt").write_text(host_pid + "\n")
        (work / "preludes.txt").write_text("".join(f"{role}\t{name}\t{digest}\n" for role, name, digest in preludes))
        result = subprocess.run(
            [sys.executable, str(reporter), "bundle", str(work), PKG, f"{PKG}/.GameActivity", "Pixel", "34", "20",
             alive, work_dir],
            capture_output=True, text=True)
        if result.returncode != 0:
            raise SystemExit(f"{label}: the reporter failed:\n{result.stderr}")
        return result.stdout


def check(name: str, condition: bool, detail: str = "") -> None:
    global failures
    if condition:
        print(f"  ok   {name}")
    else:
        failures += 1
        print(f"  FAIL {name}{': ' + detail if detail else ''}")


def verdict(text: str) -> str:
    section = text.split("## 3. Verdict", 1)[1]
    return section.strip().splitlines()[0]


clean = run("clean", [RUNNING])
check("a session that ran and painted a changing scene runs", verdict(clean).startswith("**Runs**"), verdict(clean))

died = run("died", [
    RUNNING,
    "10-19 05:00:02.000  4321  4340 E [migo]  : [Host 1] event loop error: code=JsException, msg=run_event_loop, "
    "detail=Some(\"Error: the level failed to load\\n    at game.js:40:11\")",
])
check("a game that painted and then died in a callback is blocked, not running",
      verdict(died).startswith("**Blocked**"), verdict(died))
check("and the line that says so is the blocker", "an exception escaped a callback" in died.split("## 3. Verdict")[1])

entry = run("entry", [
    RUNNING,
    "10-19 05:00:01.500  4321  4340 E [migo]  : [Host 1] handle_command failed: e=[ModuleLoadError] module evaluation "
    "failed (ReferenceError: notDefinedAnywhere is not defined",
])
check("an entry that threw is blocked", verdict(entry).startswith("**Blocked**"), verdict(entry))

rejected = run("rejected", [
    RUNNING,
    "10-19 05:00:02.000  4321  4340 E [migo]  : Unhandled Promise Rejection: Error: a request the game gave up on",
])
check("a rejection the game survived is listed and does not block",
      "a rejection nobody handled" in rejected and verdict(rejected).startswith("**Runs**"), verdict(rejected))

refused = run("refused", [
    RUNNING,
    "10-19 05:00:02.000  4321  4340 E [migo]  : Failed: Error: getBatteryInfo:fail not supported",
])
check("a call the host refused is named", "answered \"not supported\": `getBatteryInfo`" in refused)

crashed = run("crashed", [RUNNING, "10-19 05:00:03.000  4321  4321 E AndroidRuntime: FATAL EXCEPTION: main"], alive="")
check("a host that crashed is not runnable", verdict(crashed).startswith("**Not runnable as-is**"), verdict(crashed))

idle = run("idle", [])
check("no session line at all is no engine session", verdict(idle).startswith("**No engine session ran**"), verdict(idle))

elsewhere = run("elsewhere", [RUNNING], front="com.android.launcher/.Launcher")
check("another app in front is nothing observed", verdict(elsewhere).startswith("**Nothing observed**"),
      verdict(elsewhere))

recorded = run("recorded", [
    RUNNING,
    '10-19 05:00:01.100  4321  4340 I [migo]  : __MIGO_COMPAT__{"kind":"start","version":1}',
    '10-19 05:00:01.200  4321  4340 I [migo]  : __MIGO_COMPAT__{"kind":"call","surface":"wx","api":"createCanvas"}',
    '10-19 05:00:01.300  4321  4340 I [migo]  : __MIGO_COMPAT__{"kind":"first-frame","ms":120.5}',
])
check("a log the recorder wrote into adds what the game reached for",
      "## 4. What it reached for" in recorded and "`wx.createCanvas` | verified" in recorded)
check("and a log without it says nothing about reach", "What it reached for" not in clean)

ADAPTER = ("adapter", "migo-wx-adapter.bundle.js", "a" * 64)
RECORDER = ("recorder", "recorder.js", "b" * 64)


def injected(name: str, digest: str) -> str:
    return f"10-19 05:00:00.900  4321  4321 I PrescreenProbe: injected {name} sha256={digest}"


check("a run that asked for nothing says so", "- preludes asked of the host: none" in clean)
confirmed = run("confirmed", [injected("<prescreen-adapter>", ADAPTER[2]), RUNNING], preludes=(ADAPTER,))
check("an adapter the host logged injecting is called injected",
      "adapter `migo-wx-adapter.bundle.js` (sha256 `aaaaaaaaaaaa`): injected, by the host's log" in confirmed)
check("and nothing doubts it", "did not confirm" not in confirmed)
ignored = run("ignored", [
    RUNNING,
    "10-19 05:00:01.500  4321  4340 E [migo]  : [Host 1] handle_command failed: e=[ModuleLoadError] module evaluation "
    "failed (ReferenceError: wx is not defined",
], preludes=(ADAPTER,))
check("an adapter the host never logged is not called injected",
      "**asked for, not confirmed**" in ignored and "injected, by the host's log" not in ignored)
check("and the report says what follows is about the run", "The host did not confirm `migo-wx-adapter.bundle.js`" in ignored)
other = run("other", [injected("<prescreen-adapter>", "c" * 64), RUNNING], preludes=(ADAPTER,))
check("another file injected does not confirm this one", "**asked for, not confirmed**" in other)
silent = run("silent", [injected("<migo-compat-recorder>", RECORDER[2]), RUNNING], preludes=(RECORDER,))
refusal = run("refusal", [
    f"10-19 05:00:00.900  4321  4321 E PrescreenProbe: not injected <prescreen-adapter> sha256={ADAPTER[2]}", RUNNING,
], preludes=(ADAPTER,))
check("a host saying it did not inject is not confirmation", "**asked for, not confirmed**" in refusal)
echoed = run("echoed", [
    f"10-19 05:00:01.100  4321  4340 I [migo]  : injected <prescreen-adapter> sha256={ADAPTER[2]}", RUNNING,
], preludes=(ADAPTER,))
check("the content's console saying the words is not confirmation", "**asked for, not confirmed**" in echoed)
relayed = run("relayed", [
    f"10-19 05:00:01.100  4321  4340 I [migo]  : PrescreenProbe: injected <prescreen-adapter> sha256={ADAPTER[2]}",
    RUNNING,
], preludes=(ADAPTER,))
check("the content's console spelling a host's line is not confirmation", "**asked for, not confirmed**" in relayed)
denied = run("denied", [
    f"10-19 05:00:00.900  4321  4321 E PrescreenProbe: not injected: injected <prescreen-adapter> sha256={ADAPTER[2]}",
    RUNNING,
], preludes=(ADAPTER,))
check("a refusal that goes on to quote the words is not confirmation", "**asked for, not confirmed**" in denied)
# logcat repeats a message's own header on each of its lines, so a console message that spells a whole host line
# after a newline arrives under the content's tag.
forged = run("forged", [
    "10-19 05:00:01.100  4321  4340 I [migo]  : x",
    f"10-19 05:00:01.100  4321  4340 I [migo]  : 10-19 05:00:00.900  1  1 I PrescreenProbe: injected <prescreen-adapter> "
    f"sha256={ADAPTER[2]}",
    RUNNING,
], preludes=(ADAPTER,))
check("a console line spelling a whole host line is not confirmation", "**asked for, not confirmed**" in forged)
elsewhere_pid = run("elsewhere pid", [injected("<prescreen-adapter>", ADAPTER[2]).replace("4321  4321", "9876  9876"),
                                     RUNNING], preludes=(ADAPTER,))
check("another process's injection is not this host's", "**asked for, not confirmed**" in elsewhere_pid)
no_pid = run("no pid", [injected("<prescreen-adapter>", ADAPTER[2]), RUNNING], preludes=(ADAPTER,), host_pid="")
check("with no host process found, nothing is confirmed, and it says why",
      "the host's process was not found" in no_pid and "injected, by the host's log" not in no_pid)
# A carriage return, though, logcat leaves inside the message.
returned = run("returned", [
    f"10-19 05:00:01.100  4321  4340 I [migo]  : \r10-19 05:00:00.900  1  1 I PrescreenProbe: injected <prescreen-adapter> "
    f"sha256={ADAPTER[2]}",
    RUNNING,
], preludes=(ADAPTER,))
check("nor one that starts it after a carriage return", "**asked for, not confirmed**" in returned)
crlf = run("crlf", [injected("<prescreen-adapter>", ADAPTER[2]) + "\r", RUNNING], preludes=(ADAPTER,))
check("a host line ending in CRLF still confirms", "injected, by the host's log" in crlf)
spaced = run("spaced", [injected("<prescreen-adapter>", ADAPTER[2]), RUNNING],
             preludes=(("adapter", "my adapter.js", ADAPTER[2]),))
check("an adapter whose file name has a space is reported", "adapter `my adapter.js`" in spaced)
check("a recorder injected whose lines are not in the log leaves nothing to read",
      "## 4. What it reached for" in silent and "**Nothing to read.**" in silent)
check("and says the host injected it but logs above INFO", "The host injected the recorder" in silent)
unasked = run("unasked", [RUNNING], preludes=(RECORDER,))
check("a recorder asked for and never confirmed says the host may not have injected it",
      "a host that did not inject it" in unasked)

print("PASS" if failures == 0 else f"FAIL: {failures} check(s) failed")
sys.exit(1 if failures else 0)
PY
