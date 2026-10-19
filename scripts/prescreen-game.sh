#!/usr/bin/env bash
# What API surface does a mini-game bundle need, and does this build have it?
#
# The question a platform asks before migrating a catalogue is "how many of my
# games break". This answers the part that can be answered without running
# anything: which `wx.*` / `migo.*` names the bundle references, and which of
# those this build actually publishes.
#
# WHAT THIS DOES NOT TELL YOU -- read before quoting it at a customer:
#
#   * A name being published is not a working call. `wx.login` exists on every
#     build; whether it succeeds depends on the host installing an auth handler.
#     This reports the surface, never the behaviour.
#   * Static analysis under-reports by nature. Bundles reach APIs in ways no
#     scanner can resolve -- computed keys, aliases through data structures,
#     `eval`. Those are reported in their own bucket rather than folded into
#     "supported", because a scanner that quietly under-reports produces a
#     confident "everything is supported" and that is the one wrong answer that
#     costs a customer.
#   * Migration also fails on things that are not API names at all: DOM/CSS
#     assumptions, WebGL2, subpackage layout, timing. Running the game is the
#     only way to see those, and it has to be run on Android -- the Linux player
#     has no device services, so it reports failures Android would not have.
#
# WHERE `wx` COMES FROM, AND WHY THIS SCRIPT ONCE DIED:
#
# The engine publishes no `wx` names at all -- that namespace is an adapter's
# job. migo-wx-adapter mirrors `migo.*` onto `wx` minus a small exclusion set,
# so the wx surface is real but it only exists once an adapter has been loaded.
# This script was deleted wholesale by the change that removed wx wording from
# the engine (#69), together with the conformance suite, even though its `wx`
# references are literal identifiers -- the thing that change said it would
# keep. Restoring it therefore also means fixing the assumption it was written
# under: it used to require the *engine* to publish `wx`, which is now never
# true, and would refuse to run at all.
#
# So: pass `--adapter FILE` (the adapter's IIFE bundle). The dumper injects it
# ahead of the surface probe, in the same isolate, exactly as a host does, and
# the `wx` names reported are the ones an adapter actually installed rather than
# the ones its source appears to assign. Without it, a bundle that references
# `wx.*` gets a report that says it cannot answer -- not a report that calls
# every wx name a gap.
#
# Usage:
#   bash scripts/prescreen-game.sh <bundle-dir> [--adapter FILE]
#                                  [--surface FILE] [--out FILE]
#
#   --adapter FILE  adapter bundle to load before the probe, so `wx.*` can be
#                   answered at all
#   --surface FILE  reuse a dump from scripts/dump-api-surface.sh instead of
#                   producing one (the dump costs a player run)
#   --stubs FILE    reuse a list from scripts/dump-stub-surface.sh instead of
#                   deriving one (deriving is a source scan, not a player run)
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUNDLE=""
SURFACE=""
ADAPTER=""
STUBS=""
OUT=""
# Defaults to a local reference dump, which is deliberately kept out of this
# repository -- it is a record of a mainstream mini-game platform's global, not
# anything migo publishes. Absent for anyone who has not produced one, so the
# report says so rather than quietly merging two very different buckets.
WXREF="$ROOT_DIR/tools/wx-api-diff/wx-android.json"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --adapter) ADAPTER="${2:?--adapter requires a path}"; shift 2 ;;
        --adapter=*) ADAPTER="${1#*=}"; shift ;;
        --stubs) STUBS="${2:?--stubs requires a path}"; shift 2 ;;
        --stubs=*) STUBS="${1#*=}"; shift ;;
        --surface) SURFACE="${2:?--surface requires a path}"; shift 2 ;;
        --surface=*) SURFACE="${1#*=}"; shift ;;
        --out) OUT="${2:?--out requires a path}"; shift 2 ;;
        --out=*) OUT="${1#*=}"; shift ;;
        --wx-reference) WXREF="${2:?--wx-reference requires a path}"; shift 2 ;;
        --wx-reference=*) WXREF="${1#*=}"; shift ;;
        -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
        *) BUNDLE="$1"; shift ;;
    esac
done

[[ -n "$BUNDLE" ]] || { echo "usage: $0 <bundle-dir> [--surface FILE] [--out FILE]" >&2; exit 2; }
[[ -d "$BUNDLE" ]] || { echo "not a directory: $BUNDLE" >&2; exit 2; }

if [[ -z "$SURFACE" ]]; then
    SURFACE="$(mktemp)"
    trap 'rm -f "$SURFACE"' EXIT
    echo "[prescreen] dumping this build's published surface..." >&2
    dump_args=(--out "$SURFACE")
    [[ -n "$ADAPTER" ]] && dump_args+=(--adapter "$ADAPTER")
    bash "$ROOT_DIR/scripts/dump-api-surface.sh" "${dump_args[@]}" >/dev/null
fi

# Some published names do nothing. `system/17_analytics.js` says so in its own
# header -- "All functions are no-op stubs that silently succeed" -- so a report
# that counts them as supported tells a customer their analytics will work.
if [[ -z "$STUBS" ]]; then
    STUBS="$(mktemp)"
    trap 'rm -f "$STUBS"' EXIT
    bash "$ROOT_DIR/scripts/dump-stub-surface.sh" --json --out "$STUBS" 2>/dev/null || : > "$STUBS"
fi

python3 - "$ROOT_DIR" "$BUNDLE" "$SURFACE" "$WXREF" "$STUBS" <<'PY' > "${OUT:-/dev/stdout}"
from __future__ import annotations

import json
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1]).resolve()
bundle = pathlib.Path(sys.argv[2]).resolve()
surface_path = pathlib.Path(sys.argv[3])

surface = json.loads(surface_path.read_text(encoding="utf-8"))
published_migo = set(surface.get("migo") or [])
# `wx: null` means no adapter was loaded when the surface was dumped. That is the
# engine's normal state, so it must not be an error -- but it does mean any
# `wx.*` the bundle references cannot be answered, and the report says so rather
# than counting all of them as gaps.
wx_dumped = surface.get("wx") is not None
published_wx = set(surface.get("wx") or [])
if not published_migo:
    print("ERROR: the surface dump has no `migo` names; refusing to report", file=sys.stderr)
    raise SystemExit(1)

# The wx reference table separates "this build lacks it" from "wx lacks it
# too", which are different findings. It is git-ignored, so it is absent on a
# fresh checkout -- and a report that silently merged the two buckets would
# inflate the gap count without saying why.
reference_path = pathlib.Path(sys.argv[4])
reference_wx: set[str] = set()
if reference_path.exists():
    reference_wx = set(json.loads(reference_path.read_text(encoding="utf-8")).get("wx", {}).keys())

stub_path = pathlib.Path(sys.argv[5]) if len(sys.argv) > 5 else None
stubbed: set[str] = set()
# What each stub actually does, in the words of the marker beside it. The report
# used to print bare names under one sentence covering all of them, and no single
# sentence is true of all of them: "succeeds silently, so a bundle that depends
# on it does not work" describes `reportEvent` exactly and libels
# `getPrivacySetting`, which answers -- correctly -- that this build has no
# privacy gate, letting content's flow proceed as designed.
stub_describes: dict[str, str] = {}
if stub_path and stub_path.exists():
    raw = stub_path.read_text(encoding="utf-8").strip()
    if raw.startswith("{"):
        payload = json.loads(raw)
        stubbed = set(payload.get("stubs", []))
        stub_describes = payload.get("describes", {})
    else:
        # A hand-supplied --stubs list stays readable: one name per line.
        stubbed = {line.strip() for line in raw.splitlines() if line.strip()}

sources = sorted(p for p in bundle.rglob("*.js") if p.is_file())
if not sources:
    print(f"ERROR: no .js under {bundle}; nothing to scan", file=sys.stderr)
    raise SystemExit(1)

IDENT = r"[A-Za-z_$][A-Za-z0-9_$]*"

# Names a bundle can bind to the namespace object. Once `const a = wx` appears,
# `a.foo` is a reference to `wx.foo`; missing that is the difference between a
# real answer and a confident wrong one on any minified bundle.
#
# The trailing guard is load-bearing. `\b` alone also matches `const canvas =
# migo.createCanvas()`, which binds a *canvas*, not the namespace -- and then
# every `canvas.getContext` in the file is reported as a missing `migo.getContext`.
# Caught on the first real bundle this was pointed at: three gaps reported,
# none of them real. Over-reporting is not the safe direction; a prescreen whose
# gaps are imaginary is worth less than none, because the customer checks.
ALIAS = re.compile(
    rf"(?:const|let|var)\s+({IDENT})\s*=\s*(?:globalThis\s*\.\s*)?(wx|migo)\s*(?![\w$.\[(])"
)

DESTRUCTURE = re.compile(
    rf"(?:const|let|var)\s*\{{(?P<body>[^}}]*)\}}\s*=\s*(?:globalThis\s*\.\s*)?(?P<ns>wx|migo)\b"
)

# Access this scanner cannot resolve. Reported, never guessed at.
OPAQUE_PATTERNS = [
    (re.compile(rf"\b(?:wx|migo)\s*\[\s*(?!['\"])"), "computed member access -- `ns[expr]`"),
    (re.compile(r"\bObject\s*\.\s*(?:keys|getOwnPropertyNames|entries)\s*\(\s*(?:wx|migo)\b"),
     "reflection over the namespace"),
    (re.compile(r"\bfor\s*\(\s*(?:const|let|var)?\s*[A-Za-z_$][A-Za-z0-9_$]*\s+in\s+(?:wx|migo)\b"),
     "for-in over the namespace"),
    (re.compile(r"\beval\s*\("), "eval"),
]


def strip_comments(text: str) -> str:
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.DOTALL)
    return "\n".join(line.split("//", 1)[0] for line in text.splitlines())


def blank_strings(text: str) -> tuple[str, list[str]]:
    """Blank out string literal *contents*, keeping length and line numbers.

    A namespace name inside a string is not a call. Real bundles are full of
    them -- a storage key "wx.cn.minigame.iap", an Android packageName
    "com.wx.minigame" -- and counting those as referenced APIs is how a report
    tells a customer they have gaps they do not have. Found on a real bundle:
    of four reported gaps, two were strings.

    What was blanked is returned rather than discarded, because silently
    dropping matches is the other way to be confidently wrong. The report lists
    them separately so a human can see they were considered.
    """
    out: list[str] = []
    hidden: list[str] = []
    i, length = 0, len(text)
    quote = ""
    buffer: list[str] = []
    while i < length:
        ch = text[i]
        if quote:
            if ch == "\\" and i + 1 < length:
                buffer.append(text[i : i + 2])
                out.append("  ")
                i += 2
                continue
            if ch == quote:
                joined = "".join(buffer)
                if re.search(r"\b(?:wx|migo)\s*\.", joined):
                    hidden.append(joined[:120])
                buffer = []
                quote = ""
                out.append(ch)
            elif ch == "\n":
                # An unterminated literal would otherwise swallow the rest of the
                # file; a newline ends every quote form except a template.
                if quote != "`":
                    quote = ""
                    buffer = []
                out.append(ch)
            else:
                buffer.append(ch)
                out.append(" " if ch != "\n" else "\n")
            i += 1
            continue
        if ch in "'\"`":
            quote = ch
            out.append(ch)
            i += 1
            continue
        out.append(ch)
        i += 1
    return "".join(out), hidden


referenced: dict[str, set[str]] = {"wx": set(), "migo": set()}
# Names the bundle *assigns* onto the namespace. `wx.__loadSubpackage__ =
# wx.loadSubpackage` is the content installing its own alias -- the opposite of
# a gap -- and reading it as a reference reports a missing API the runtime was
# never asked for. Found on a real bundle.
installed: dict[str, set[str]] = {"wx": set(), "migo": set()}
in_strings: list[tuple[str, str]] = []
opaque: list[tuple[str, int, str]] = []
alias_names: dict[str, str] = {}

texts: dict[pathlib.Path, str] = {}
# Bracket access with a literal key -- `wx["createCanvas"]` -- *is* a resolvable
# reference, and its key lives inside a string. So that one form is matched
# against the un-blanked text; everything else uses the blanked copy, where a
# name inside a string is correctly invisible. Blanking first and matching after
# turned this form into a name made of spaces, which the fixture caught.
raw_texts: dict[pathlib.Path, str] = {}
for path in sources:
    try:
        text = strip_comments(path.read_text(encoding="utf-8", errors="replace"))
    except OSError as error:
        print(f"ERROR: cannot read {path}: {error}", file=sys.stderr)
        raise SystemExit(1)
    raw_texts[path] = text
    text, hidden = blank_strings(text)
    for snippet in hidden:
        in_strings.append((str(path.relative_to(bundle)), snippet))
    texts[path] = text
    for match in ALIAS.finditer(text):
        alias_names[match.group(1)] = match.group(2)

for path, text in texts.items():
    rel = path.relative_to(bundle)
    for ns in ("wx", "migo"):
        for match in re.finditer(rf"\b{ns}\s*\.\s*({IDENT})", text):
            # `ns.name = ...` is an assignment (but `==`, `===` and `=>` are not).
            tail = text[match.end() : match.end() + 3]
            if re.match(r"\s*=(?![=>])", tail):
                installed[ns].add(match.group(1))
            else:
                referenced[ns].add(match.group(1))
        for match in re.finditer(
            rf"\b{ns}\s*\[\s*['\"]([^'\"]+)['\"]\s*\]", raw_texts[path]
        ):
            referenced[ns].add(match.group(1))
    for match in DESTRUCTURE.finditer(text):
        ns = match.group("ns")
        for part in match.group("body").split(","):
            name = part.split(":")[0].strip()
            if re.fullmatch(IDENT, name):
                referenced[ns].add(name)
    for alias, ns in alias_names.items():
        for match in re.finditer(rf"\b{re.escape(alias)}\s*\.\s*({IDENT})", text):
            referenced[ns].add(match.group(1))
    for pattern, reason in OPAQUE_PATTERNS:
        for match in pattern.finditer(text):
            line = text.count("\n", 0, match.start()) + 1
            opaque.append((str(rel), line, reason))

# What game.json asks the platform for that is not an API. A plugin is served by the platform, from its own
# servers, to `requirePlugin(...)`; this build has neither the service nor the function, so a bundle that declares
# one does not start -- and no name scan sees it, because `requirePlugin` is a global, not a `wx.*` name. Cocos
# Creator delivers its engine this way when its "Separate Engine" option is on for the platform's mini-games.
plugins: dict = {}
manifest_path = bundle / "game.json"
if manifest_path.is_file():
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (ValueError, UnicodeDecodeError):
        manifest = None
    if isinstance(manifest, dict) and isinstance(manifest.get("plugins"), dict):
        plugins = manifest["plugins"]
# Declaring a plugin is not loading one: a bundle may list a plugin it never asks for. Read with comments
# and strings blanked, so only a call in code counts.
plugin_callers = sorted(str(path.relative_to(bundle)) for path, text in texts.items()
                        if re.search(r"\brequirePlugin\s*\(", text))

# A name the bundle both installs and calls is its own polyfill, not a gap.
wx_used = referenced["wx"] - installed["wx"]
migo_used = referenced["migo"] - installed["migo"]
wx_missing = sorted(wx_used - published_wx)
migo_missing = sorted(migo_used - published_migo)
wx_ok = sorted(wx_used & published_wx)

# A name this build lacks reads very differently depending on whether wx has it:
# one is a gap in this runtime, the other is content calling something that does
# not exist on wx either (a typo, an adapter shim, a different platform's API).
missing_in_wx_too = [n for n in wx_missing if reference_wx and n not in reference_wx]
missing_but_wx_has = [n for n in wx_missing if not reference_wx or n in reference_wx]

# Without an adapter surface there is nothing to compare `wx.*` against, so every
# wx bucket is empty rather than "everything is missing". Cleared here, next to
# where they are computed -- clearing them further down, in the middle of
# emitting the report, is how the next edit puts the wrong answer back.
if not wx_dumped:
    wx_missing, missing_in_wx_too, missing_but_wx_has, wx_ok = [], [], [], []

out = []
out.append(f"# Prescreen: `{bundle.name}`")
out.append("")
out.append(f"- bundle: `{bundle}`")
out.append(f"- scanned: {len(sources)} JavaScript file(s)")
if wx_dumped:
    out.append(f"- this build publishes: {len(published_migo)} `migo` names; with the adapter loaded, {len(published_wx)} `wx` names")
else:
    out.append(f"- this build publishes: {len(published_migo)} `migo` names; **no adapter was loaded**, so `wx.*` is unanswered")
out.append("")
out.append("## Summary")
out.append("")
out.append("| | count |")
out.append("|---|---:|")
out.append(f"| `wx.*` names referenced | {len(wx_used)} |")
if not wx_dumped:
    out.append("| of those, available | *unanswered — no adapter loaded* |")
elif True:
    out.append(f"| of those, published by this build | {len(wx_ok)} |")
if wx_dumped and reference_wx:
    out.append(f"| **not published, and wx has it** | **{len(missing_but_wx_has)}** |")
    out.append(f"| not published, and not a wx API either | {len(missing_in_wx_too)} |")
elif wx_dumped:
    out.append(f"| **not published by this build** | **{len(missing_but_wx_has)}** |")
    out.append("| of those, how many wx has | *unknown -- no reference table* |")
stub_hits = sorted((wx_used | migo_used) & stubbed)
out.append(f"| **referenced, published, but a stub** | **{len(stub_hits)}** |")
out.append(f"| `migo.*` names not published | {len(migo_missing)} |")
out.append(f"| **sites this scanner cannot resolve** | **{len(opaque)}** |")
out.append(f"| **plugins `game.json` asks the platform to serve** | **{len(plugins)}** |")
out.append("")

if plugins:
    out.append("## Plugins the platform would serve")
    out.append("")
    if plugin_callers:
        out.append(
            f"`game.json` declares **{len(plugins)} plugin(s)**, which the mini-game platform serves from its own "
            "servers to `requirePlugin(...)`, and the bundle calls `requirePlugin` in "
            + ", ".join(f"`{name}`" for name in plugin_callers)
            + ". This build has no plugin service and no `requirePlugin`, so the bundle does not start as built. "
            "Cocos Creator delivers its engine this way when \"Separate Engine\" is on: rebuild with it off, so the "
            "engine ships inside the game."
        )
    else:
        out.append(
            f"`game.json` declares **{len(plugins)} plugin(s)**, which the mini-game platform serves from its own "
            "servers to `requirePlugin(...)`, and **no file scanned calls `requirePlugin`**: the declaration may be "
            "unused. A call this scan cannot see (computed, or through `eval`) would fail, because this build has no "
            "plugin service."
        )
    out.append("")
    for name in sorted(plugins):
        entry = plugins[name] if isinstance(plugins[name], dict) else {}
        details = ", ".join(f"{key} `{entry[key]}`" for key in ("provider", "version") if key in entry)
        out.append(f"- `{name}`" + (f" -- {details}" if details else ""))
    out.append("")

if wx_used and not wx_dumped:
    out.append("## `wx.*` could not be answered — no adapter was loaded")
    out.append("")
    out.append(
        f"This bundle references **{len(wx_used)} `wx.*` names**, and the surface dump "
        "used here contains no `wx` at all. That is not a finding about the bundle: "
        "**the engine publishes no `wx` names by design** — the namespace is installed "
        "by an adapter, which mirrors `migo.*` onto `wx`."
    )
    out.append("")
    out.append(
        "Reporting these as gaps would produce exactly the confident wrong answer this "
        "tool exists to avoid, so they are not counted. Re-run with the adapter:"
    )
    out.append("")
    out.append("```sh")
    out.append("bash scripts/prescreen-game.sh <bundle> \\")
    out.append("  --adapter path/to/migo-wx-adapter.bundle.js")
    out.append("```")
    out.append("")

if wx_dumped and not reference_wx and wx_missing:
    out.append("## wx reference table unavailable")
    out.append("")
    out.append(
        f"`{reference_path}` was not found, so this report **cannot separate** "
        "\"this build lacks it\" from \"wx lacks it too\". Every unpublished name "
        "below is listed as a gap, which overstates them: some are likely adapter "
        "shims or other platforms' SDKs that wx never had either."
    )
    out.append("")
    out.append(
        "The table is produced by dumping the global of a mainstream mini-game "
        "runtime and is kept out of this repository; pass one with "
        "`--wx-reference FILE` if you have it."
    )
    out.append("")

if missing_but_wx_has:
    header = (
        "## Referenced, wx has it, this build does not"
        if reference_wx
        else "## Referenced but not published by this build"
    )
    out.append(header)
    out.append("")
    out.append(
        "Each of these is a real gap for this bundle."
        if reference_wx
        else "Unclassified -- see the note above about the missing reference table."
    )
    out.append("")
    for name in missing_but_wx_has:
        out.append(f"- `wx.{name}`")
    out.append("")

if missing_in_wx_too:
    out.append("## Referenced but not a wx API either")
    out.append("")
    out.append(
        "Usually an adapter shim, another platform's SDK, or dead code. Worth a "
        "look, but not evidence of a gap in this runtime."
    )
    out.append("")
    for name in missing_in_wx_too:
        out.append(f"- `{name}`")
    out.append("")

if migo_missing:
    out.append("## `migo.*` names not published")
    out.append("")
    for name in migo_missing:
        out.append(f"- `migo.{name}`")
    out.append("")

if in_strings:
    out.append("## Namespace-looking text inside string literals")
    out.append("")
    out.append(
        f"{len(in_strings)} string(s) contain something shaped like `wx.` or `migo.`. "
        "These are **not** counted as references — a storage key or an Android "
        "package name is not an API call — but they are listed so the omission is "
        "visible rather than silent."
    )
    out.append("")
    seen_s: set[tuple[str, str]] = set()
    shown = 0
    for rel, snippet in in_strings:
        key = (rel, snippet)
        if key in seen_s:
            continue
        seen_s.add(key)
        if shown >= 20:
            continue
        out.append(f"- `{rel}` — `{snippet}`")
        shown += 1
    if len(seen_s) > 20:
        out.append(f"- ... and {len(seen_s) - 20} more")
    out.append("")

if installed["wx"] or installed["migo"]:
    total_installed = len(installed["wx"]) + len(installed["migo"])
    out.append("## Names the bundle installs itself")
    out.append("")
    out.append(
        f"{total_installed} name(s) are **assigned** onto the namespace by the bundle "
        "rather than called on it — a polyfill, an alias, or a shim it carries. Not "
        "gaps: the runtime was never asked for them."
    )
    out.append("")
    for ns in ("wx", "migo"):
        for name in sorted(installed[ns]):
            out.append(f"- `{ns}.{name}`")
    out.append("")

if stub_hits:
    out.append("## Published, but they do nothing")
    out.append("")
    out.append(
        "These names exist on this build and this bundle calls them, so they do not "
        "appear as gaps above — **and none of them is implemented**. What that "
        "costs you differs per name, so each is quoted below in the words of the "
        "marker beside it in the engine source. Some fail loudly and your code can "
        "detect them. Some accept the call and drop it, which is invisible here and "
        "in your logs. And some report, truthfully, that this build does not have a "
        "capability — those are not breakage, and reading them as breakage is the "
        "mistake this list is arranged to prevent."
    )
    out.append("")
    for name in stub_hits:
        description = stub_describes.get(name, "").strip()
        if description:
            out.append(f"- `{name}` — {description}")
        else:
            out.append(f"- `{name}`")
    out.append("")
    out.append(
        "The list is derived from the engine sources by "
        "`scripts/dump-stub-surface.sh`, not maintained by hand, so it cannot drift "
        "from what the build actually does."
    )
    out.append("")
elif stubbed:
    out.append("## Published, but they do nothing")
    out.append("")
    out.append(
        f"None. This build has {len(stubbed)} published names that are stubs, and "
        "this bundle references none of them."
    )
    out.append("")

out.append("## Sites this scanner cannot resolve")
out.append("")
if not opaque:
    out.append(
        "None. Every namespace access in this bundle is a literal name, so the "
        "counts above cover what it references."
    )
else:
    out.append(
        f"{len(opaque)} site(s). The bundle reaches the namespace in ways static "
        "analysis cannot follow, so **the counts above are a lower bound** -- it "
        "may use names that do not appear here."
    )
    out.append("")
    seen = set()
    for rel, line, reason in opaque[:40]:
        key = (rel, reason)
        if key in seen:
            continue
        seen.add(key)
        out.append(f"- `{rel}:{line}` — {reason}")
    if len(opaque) > 40:
        out.append(f"- ... and {len(opaque) - 40} more")
out.append("")

out.append("## What this report is not")
out.append("")
out.append(
    "- **Not a compatibility verdict.** A published name is not a working call: "
    "`wx.login` exists on every build and still fails without a host auth handler."
)
out.append(
    "- **Not a migration estimate.** DOM/CSS assumptions, WebGL2, subpackage "
    "layout and timing break games without any missing API name."
)
out.append(
    "- **Not a substitute for running it.** And it has to run on Android: the "
    "Linux player has no device services, so it reports failures Android would "
    "not have."
)

print("\n".join(out))
PY

if [[ -n "$OUT" ]]; then
    echo "[prescreen] report -> $OUT" >&2
fi
