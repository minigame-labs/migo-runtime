#!/usr/bin/env bash
# The host reaches content through a handle, never through a name.
#
# Host callbacks used to travel as eval'd JavaScript naming
# `globalThis[Symbol.for('Migo.hostBridge')]`. `Symbol.for` reads the *global*
# symbol registry, so content asked for the same symbol and reached all 78
# hooks behind it -- including `_internalOnAdEvent`, through which it could
# forge a rewarded-video completion.
#
# That is closed: `js_bindings` resolves the holder once at start-up, keeps a
# handle, and deletes the symbol from the global object. A handle needs no
# name, so the host still reaches every hook and content reaches none.
#
# This gate protects the *mechanism*; the behaviour is covered by
# `runtime-v8/src/tests/host_bridge_dispatch.rs`. The split matters: tests catch
# a dispatcher that stops working, and this catches the channel being rebuilt
# alongside it -- a new callback added the old way would pass every test in the
# suite while re-opening exactly what was closed.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

python3 - "$ROOT_DIR" <<'PY'
from __future__ import annotations

import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1]).resolve()

crates = root / "engine/crates"
bindings_rs = crates / "runtime-v8/src/js_bindings.rs"
main_js = crates / "runtime-v8/src/99_main.js"

for required in (crates, bindings_rs, main_js):
    if not required.exists():
        print(f"ERROR: {required} not found; this gate cannot check anything", file=sys.stderr)
        sys.exit(1)


def strip_comments(text: str) -> str:
    """A name discussed in a comment is not a name in the code.

    Every doc block below explains this channel at length, and a grep that sees
    those reports the mechanism as present after it has been removed -- or as
    reintroduced when someone merely wrote about it.
    """
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.DOTALL)
    return "\n".join(re.sub(r"//.*", "", line) for line in text.splitlines())


HOLDER = "Migo.hostBridge"
failures: list[str] = []

# --- 1. No Rust builds JS source that names the holder --------------------
#
# Tests are exempt and deliberately so: they simulate the host at a layer below
# `js_bindings`, before the name is retired, and one of them asserts that it is
# retired. Excluding them costs nothing here because production code reaching
# the holder by name is what this looks for, and that cannot hide in a test.
offenders: list[str] = []
for source_path in sorted(crates.rglob("*.rs")):
    rel = source_path.relative_to(root)
    if "/tests/" in str(rel) or source_path == bindings_rs:
        continue
    text = strip_comments(source_path.read_text(encoding="utf-8", errors="replace"))
    for index, line in enumerate(text.splitlines(), start=1):
        if HOLDER in line:
            offenders.append(f"{rel}:{index}")

if offenders:
    failures.append(
        "these name the host-bridge holder in code, which puts the host back on a "
        "channel content can also use -- route the callback through "
        "`HostCommand::InvokeHostHook` instead:\n      "
        + "\n      ".join(offenders)
    )

# --- 2. The JS-source builders stay gone ----------------------------------
#
# They existed only to phrase a host callback as source. Their return is the
# regression, whether or not anything calls them yet.
for gone in ("HOST_BRIDGE_EXPR", "build_eval_script"):
    hits = [
        str(p.relative_to(root))
        for p in sorted(crates.rglob("*.rs"))
        if re.search(rf"\b(pub\s+)?(const|fn)\s+{gone}\b", strip_comments(p.read_text(encoding="utf-8", errors="replace")))
    ]
    if hits:
        failures.append(f"`{gone}` is back ({', '.join(hits)}); it builds the channel this gate closed")

# --- 3. js_bindings resolves, retains, and retires -------------------------
#
# All three or none: resolving without retiring leaves the name reachable;
# retiring without retaining kills every callback on the next reload, silently,
# because the fallback path finds an empty global instead.
bindings = strip_comments(bindings_rs.read_text(encoding="utf-8"))
for anchor, why in (
    (r"\bbridge_holder\b", "the holder is not retained, so a reload has nothing to fall back on"),
    (r"\bfn\s+retire_bridge_name\b", "nothing removes the holder's name from the global object"),
    (r"\bglobal\.delete\s*\(", "`retire_bridge_name` no longer deletes anything"),
    (r"Symbol::for_key", "the holder is no longer resolved through the global symbol registry"),
):
    if not re.search(anchor, bindings):
        failures.append(f"{bindings_rs.relative_to(root)}: {why}")

# A defined-but-uncalled retirement leaves the name installed while every
# anchor above still matches. Checked by removing the declaration and looking
# for the identifier again: what is left can only be a use.
if not re.search(r"\bretire_bridge_name\b", re.sub(r"\bfn\s+retire_bridge_name\b", "", bindings)):
    failures.append(
        f"{bindings_rs.relative_to(root)}: `retire_bridge_name` is defined but never "
        "called, so the holder keeps the name it was meant to lose"
    )

# --- 4. The holder property has to be deletable ---------------------------
#
# `delete` on a non-configurable property is a silent no-op, so this would fail
# as "content can still reach the holder" with everything else looking correct.
holder_block = re.search(
    r"Object\.defineProperty\(\s*globalThis,\s*Symbol\.for\(\s*\"Migo\.hostBridge\"\s*\)\s*,\s*\{(?P<body>.*?)\}\s*\)",
    strip_comments(main_js.read_text(encoding="utf-8")),
    re.DOTALL,
)
if not holder_block:
    failures.append(
        f"{main_js.relative_to(root)}: the holder installation was not found; "
        "this gate can no longer tell whether it is deletable"
    )
elif not re.search(r"configurable:\s*true", holder_block.group("body")):
    failures.append(
        f"{main_js.relative_to(root)}: the holder is installed non-configurable, "
        "which makes retiring its name a silent no-op"
    )

# --- 5. The Performance+ producer retires the name too ----------------------
#
# On iOS Performance+ the same engine JavaScript installs the same holder in a
# Worker in WebKit, and the producer is what stands where `js_bindings` stands:
# it must take what it needs and delete the name after the engine loads and
# before the game is imported. It did not, until 2026-09-19: content on that
# lane could reach every hook.
producer = root / "platforms/apple/WebContent/PerformancePlus/src/producer-worker.mjs"
worker = strip_comments(producer.read_text(encoding="utf-8"))
boot = worker.find('import("./engine/boot.mjs")')
retire = re.search(r"delete\s+globalThis\[\s*bridgeName\s*\]", worker)
named = re.search(r"const\s+bridgeName\s*=\s*Symbol\.for\(\s*\"Migo\.hostBridge\"\s*\)", worker)
game = worker.find("config.gameEntry")
if boot < 0 or retire is None or named is None:
    failures.append(
        f"{producer.relative_to(root)}: the producer does not delete the host-bridge name "
        "after loading the engine, so content in WebKit can reach every hook"
    )
elif not (boot < retire.start() and (game < 0 or retire.start() < game)):
    failures.append(
        f"{producer.relative_to(root)}: the host-bridge name is deleted outside the window "
        "between loading the engine and importing the game"
    )

if failures:
    print("FAIL: host-bridge channel contract", file=sys.stderr)
    for failure in failures:
        print(f"  - {failure}", file=sys.stderr)
    sys.exit(1)

print("PASS: host callbacks travel by handle; the holder's global name is retired")
PY
