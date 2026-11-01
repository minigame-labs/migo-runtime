#!/usr/bin/env python3
"""Generate contracts/runtime/api-platform-matrix.json: the platform's API x Migo's platforms.

For every API in contracts/runtime/minigame-api.json and every platform Migo
ships, one status:

  engine       the engine answers it alone; nothing to wire.
  default      it needs the host, and the platform's own host layer (Android's
               Java SDK, the Apple Swift layer, Linux's host kit, the
               OpenHarmony host) already provides it.
  host         it needs the host, and the platform offers the interface for a
               host app to provide it, with no default yet.
  missing      no way to make it work on that platform yet.
  unsupported  deliberately not offered there (api-platform-decisions.json).

A cell is *ready* when it is engine, default or unsupported, or host where every
need is the host's by nature (its adverts, payments, accounts). A cell the
reference itself does not offer on that kind of client (minigame-api-spec.json's
clients: most of the API is phones-only, a few calls desktop-only) is *outside*:
nothing there is owed to compatibility, whatever Migo happens to provide. The matrix is
derived, not written: what an API needs comes from calling it
(minigame-api-reach.json: the ops it dispatched, the host services it asked
for); what a platform provides comes from its sources, by the FACTS below; only
what cannot be measured comes from api-platform-decisions.json.

  python3 scripts/gen-api-platform-matrix.py [--root DIR] [--check]

--check compares with the committed matrix instead of writing it. The gate is
scripts/test-api-platform-matrix-contract.sh. Standard library only.
"""

from __future__ import annotations

import argparse
import collections
import json
import pathlib
import re
import sys

PLATFORMS = ("android", "ios", "macos", "linux", "windows", "ohos")
# Worst first: a cell is as good as its worst need.
RANK = ("missing", "host", "default", "engine")

REFERENCE = "contracts/runtime/minigame-api.json"
SPEC = "contracts/runtime/minigame-api-spec.json"
# Which of the reference's clients each platform is held to. Its phones offer every API but the few its pages say are
# desktop-only (clients.mobile false); its Windows, Mac and HarmonyOS clients offer what their notes say. There is no
# Linux client: a Linux desktop is held to what the reference offers on a desktop, either one.
REFERENCE_CLIENTS = {
    "android": ("mobile",),
    "ios": ("mobile",),
    "macos": ("mac",),
    "linux": ("mac", "windows"),
    "windows": ("windows",),
    "ohos": ("harmony",),
}
REACH = "contracts/runtime/minigame-api-reach.json"
DECISIONS = "contracts/runtime/api-platform-decisions.json"
# What migo-conformance's api-spec bundle measured on each platform (scripts/import-api-conformance.py); absent for a
# platform no runner has measured.
CONFORMANCE = "contracts/runtime/api-conformance-{platform}.json"
CONFORMANCE_STATUSES = ("verified", "failing", "present", "absent")
HOST_SERVICES = "contracts/runtime/host-services.json"
OP_BOUNDARY = "contracts/runtime/op-boundary.json"
MATRIX = "contracts/runtime/api-platform-matrix.json"

# Where each fact is read. Each reader fails when its pattern finds nothing, so
# a moved file or a renamed construct is a failure, not a silently empty fact.
CAPI_HOST_KIT = "engine/crates/capi/src/host_kit.rs"
CAPI_DEVICE = "engine/crates/capi/src/device.rs"
CAPI_CALLBACKS = "engine/crates/capi/src/callbacks.rs"
C_HEADERS = "include/migo"
ANDROID_SERVICES = "engine/crates/platform/src/android/services/mod.rs"
ANDROID_JAVA = "platforms/android/library/src/main/java"
ANDROID_HANDLERS = "platforms/android/library/src/main/java/com/migo/runtime/callback"
APPLE_LAYER = "platforms/apple/Sources/MigoAppleRenderer"
APPLE_MAC_VIEW = "platforms/apple/Sources/MigoMacV8"
APPLE_IOS_VIEW = "platforms/apple/Sources/MigoApplePerformancePlus"
# The optional capability products an app links and hands its game view.
APPLE_CAPABILITIES = ("platforms/apple/Sources/MigoAppleLocation", "platforms/apple/Sources/MigoAppleBluetooth")
LINUX_LAYER = "platforms/linux/host-kit/src"
OHOS_LAYER = "platforms/openharmony/entry/src/main"

# A host-service event the Android SDK reports through its own native rather
# than the channel: event -> the Java SDK's native for it.
ANDROID_EVENTS = {
    "screen.device_orientation_change": "onDeviceOrientationChange",
}

# Rust's target_os for the platforms the matrix names.
# What each platform's Rust targets answer to `cfg(...)`: OpenHarmony's are
# `target_os = "linux"` too, told apart by their environment.
CFG_FACTS = {
    "android": {"target_os": "android"},
    "ios": {"target_os": "ios", "target_vendor": "apple"},
    "macos": {"target_os": "macos", "target_vendor": "apple"},
    "linux": {"target_os": "linux", "target_env": "gnu"},
    "windows": {"target_os": "windows"},
    "ohos": {"target_os": "linux", "target_env": "ohos"},
}

# Host input, lifecycle and reported state, by kind: the C ABI entry point a host
# feeds it through, and the Java SDK's native for the same -- None where the
# Android SDK reads the state from the platform itself, through the engine
# service its accessor names.
INPUT = {
    "touch": ("migo_session_send_touch", "onTouchRaw"),
    "pointer": ("migo_session_send_pointer_event", "onPointerEvent"),
    "wheel": ("migo_session_send_wheel_event", "onWheelEvent"),
    "key": ("migo_session_send_key_event", "onKeyEvent"),
    "gamepad": ("migo_session_set_gamepad_connected", "onGamepadConnection"),
    "visibility": ("migo_session_set_visibility", "onShow"),
    "memory": ("migo_session_post_memory_warning", "onMemoryWarning"),
    "app_authorize": ("migo_session_set_app_authorize_setting", None),
    "system_settings": ("migo_session_set_system_settings", None),
}

# A service the Java SDK leaves to the host app: its handler interface. Every
# other service the Android SDK implements itself, but for the calls it hands to
# one of these handlers (ANDROID_DELEGATE).
ANDROID_HANDLER = {
    "ad": "AdHandler",
    "auth": "AuthHandler",
    "ecosystem": "EcosystemHandler",
    "navigate": "NavigationHandler",
    "payment": "PaymentHandler",
    "permission": "PermissionHandler",
    "setting": "SettingHandler",
    "share": "ShareHandler",
    "subpackage": "SubpackageHandler",
    "update": "UpdateHandler",
}

COMMENT = [
    "Every API of the common mini-game platform (minigame-api.json) on every platform Migo ships. GENERATED by",
    "scripts/gen-api-platform-matrix.py -- do not edit; change what it is derived from and regenerate. The gate is",
    "scripts/test-api-platform-matrix-contract.sh.",
    "",
    "engine: the engine answers it alone. default: it needs the host and the platform's own host layer provides it.",
    "host: the platform offers the interface for a host app to provide it, with no default yet. missing: no way to",
    "make it work there yet. unsupported: deliberately not offered (api-platform-decisions.json gives the reason).",
    "",
    "needs: what the API takes from a host -- channel:<service>.<method> and channel:<service> (the C ABI host-service",
    "channel), callback:<field> (a typed MigoHostCallbacks field), report:<what> (state the host reports), input:<kind>",
    "(input or lifecycle the host feeds), event:<service>.<event> (a host-service event the host posts: a listener",
    "that needs no request), service:<accessor> (an engine service no C ABI host can supply yet).",
    "wired: the cell has a working path -- engine, default, unsupported, or host for needs that are the host's by",
    "nature. verified: migo-conformance's api-spec bundle exercised it on the platform and every check passed",
    "(api-conformance-<platform>.json). ready: wired, and verified -- or deliberately unsupported, or the host app's to",
    "answer. not_ready: the platforms where the cell is not ready. summary counts each status and the wired, verified",
    "and ready cells per platform. outside: the platforms whose reference client does not offer the API",
    "(minigame-api-spec.json's clients; android/ios: the phones, macos: Mac, windows: Windows, linux: either desktop,",
    "ohos: HarmonyOS) -- nothing there is owed to compatibility, so they are not in not_ready and are counted ready.",
]


class Fail(SystemExit):
    def __init__(self, message: str) -> None:
        super().__init__(f"gen-api-platform-matrix: {message}")


def read(root: pathlib.Path, relative: str) -> str:
    path = root / relative
    if not path.exists():
        raise Fail(f"{relative} is missing")
    return path.read_text(encoding="utf-8")


def load(root: pathlib.Path, relative: str) -> dict:
    return json.loads(read(root, relative))


def found(what: str, items):
    if not items:
        raise Fail(f"no {what} found; the source it is read from has changed shape")
    return items


def cfg_holds(condition: str, facts: dict[str, str]) -> bool:
    """Whether a `cfg(...)` predicate -- `key = "value"`, `any`, `all`, `not` --
    holds for a target with these facts. ValueError for one it cannot read."""
    condition = condition.strip()
    combinator = re.fullmatch(r"(any|all|not)\((.*)\)", condition, re.S)
    if combinator:
        terms, depth, start = [], 0, 0
        inner = combinator.group(2)
        for index, char in enumerate(inner):
            depth += {"(": 1, ")": -1}.get(char, 0)
            if char == "," and depth == 0:
                terms.append(inner[start:index])
                start = index + 1
        terms = [term for term in terms + [inner[start:]] if term.strip()]
        held = [cfg_holds(term, facts) for term in terms]
        if combinator.group(1) == "not":
            if len(held) != 1:
                raise ValueError(f"cfg(not({inner})) takes one predicate")
            return not held[0]
        return any(held) if combinator.group(1) == "any" else all(held)
    pair = re.fullmatch(r'(target_os|target_env|target_vendor) = "(\w+)"', condition)
    if not pair:
        raise ValueError(f"cfg predicate {condition!r} does not say which platforms")
    return facts.get(pair.group(1)) == pair.group(2)


def braced(text: str, start: int) -> str:
    """The body of the block whose `{` is at or after `start`."""
    open_at = text.index("{", start)
    depth = 0
    for index in range(open_at, len(text)):
        if text[index] == "{":
            depth += 1
        elif text[index] == "}":
            depth -= 1
            if depth == 0:
                return text[open_at + 1 : index]
    raise Fail("unbalanced braces")


# ---- facts: the C ABI ---------------------------------------------------------


def channel_ops(root: pathlib.Path) -> dict[str, tuple[str, str]]:
    """op -> (service, method) on the host-service channel."""
    services = load(root, HOST_SERVICES)["services"]
    ops = {}
    for service, entry in services.items():
        for method, spec in entry.get("methods", {}).items():
            if "op" in spec:
                ops[spec["op"]] = (service, method)
    return found("host-service ops", ops)


def cfg_before(text: str) -> str | None:
    """The condition of the `#[cfg(...)]` that `text` ends with -- doc comments
    and whitespace after it aside -- or None."""
    tail = re.search(r"\)\]\s*(?:///[^\n]*\n\s*)*$", text)
    if not tail:
        return None
    depth, index = 0, tail.start()
    while index >= 0:
        depth += {")": 1, "(": -1}.get(text[index], 0)
        if depth == 0:
            break
        index -= 1
    head = text[:index]
    if index < 0 or not head.endswith("#[cfg"):
        return None
    return text[index + 1 : tail.start()]


def capi_services(root: pathlib.Path) -> dict[str, tuple[str, str | None]]:
    """accessor -> how a C ABI host supplies it.

    ("channel", service), ("callback", field), ("report", accessor),
    ("always", None), ("native", "platform,...") for an engine-side
    implementation compiled only for some operating systems
    (`#[cfg(target_os = ...)]` on the accessor), or ("mixed", "service|field")
    for an accessor that is a typed callback for one op and the channel for the
    rest.
    """
    kit = read(root, CAPI_HOST_KIT)
    device = read(root, CAPI_DEVICE)
    callbacks = read(root, CAPI_CALLBACKS)
    supplies = dict(re.findall(r"pub fn supplies_(\w+)\(&self\) -> bool \{\s*self\.callbacks\.(on_\w+)", callbacks))
    found("callback suppliers", supplies)
    device_kinds: dict[str, tuple[str, str | None]] = {}
    for name, body in re.findall(r"pub\(crate\) fn (\w+)\(self: &Arc<Self>\)[^{]*\{(.*?)\n    \}", device, re.S):
        supplier = re.search(r"Notifier::supplies_(\w+)", body)
        if supplier:
            device_kinds[name] = ("callback", supplies[supplier.group(1)])
        else:
            device_kinds[name] = ("report", name)
    found("CapiDevice services", device_kinds)
    kinds: dict[str, tuple[str, str | None]] = {}
    for block in re.finditer(r"impl \w+ for CapiDeviceServices \{", kit):
        body = braced(kit, block.start())
        for found_fn in re.finditer(r"fn (\w+)\(&self\) -> Option<Arc<dyn \w+>> \{(.*?)\n    \}", body, re.S):
            name, accessor = found_fn.group(1), found_fn.group(2)
            # The `#[cfg(...)]` attribute right before the function, if any --
            # whole, however rustfmt wrapped it: an attribute too long for one
            # line spans several, and reading only the last would miss it.
            only = cfg_before(body[: found_fn.start()])
            if only is not None:
                # One line, as it would be written by hand: whitespace collapsed,
                # none inside the parentheses, no trailing comma.
                condition = re.sub(r"\s+", " ", only).strip()
                condition = re.sub(r"\(\s+", "(", condition)
                condition = re.sub(r",?\s*\)", ")", condition).rstrip(", ")
                try:
                    systems = {platform for platform, facts in CFG_FACTS.items() if cfg_holds(condition, facts)}
                except ValueError as error:
                    raise Fail(f"C ABI accessor {name}: {error}") from None
                if not systems:
                    raise Fail(f"C ABI accessor {name}: cfg({condition}) holds on no platform")
                # An accessor defined once per platform group: the groups together.
                if name in kinds and kinds[name][0] == "native":
                    systems |= set(kinds[name][1].split(","))
                kinds[name] = ("native", ",".join(sorted(systems)))
                continue
            channel = re.search(r"MIGO_HOST_SERVICE_(\w+)", accessor)
            typed = re.search(r"self\.device\.(\w+)\(\)", accessor)
            if channel and typed:
                field = device_kinds[typed.group(1)][1]
                kinds[name] = ("mixed", f"{channel.group(1).lower()}|{field}")
            elif channel:
                kinds[name] = ("channel", channel.group(1).lower())
            elif typed:
                kinds[name] = device_kinds[typed.group(1)]
            elif "self.keyboard" in accessor:
                kinds[name] = ("callback", supplies["keyboard"] if "keyboard" in supplies else "on_show_keyboard")
            else:
                kinds[name] = ("always", None)
    return found("C ABI services", kinds)


def c_entry_points(root: pathlib.Path) -> set[str]:
    names = set()
    for header in sorted((root / C_HEADERS).glob("*.h")):
        names |= set(re.findall(r"\b(migo_\w+)\s*\(", header.read_text(encoding="utf-8")))
    return found("C ABI entry points", names)


# ---- facts: the platforms' host layers ------------------------------------------


def swift_conditions(text: str):
    """Yield (line, platforms) with the platforms each line of Swift compiles for."""
    stack: list[set[str]] = []
    every = {"ios", "macos"}
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("#if "):
            stack.append(_swift_platforms(stripped[4:]))
            continue
        if stripped.startswith("#elseif "):
            stack[-1] = _swift_platforms(stripped[8:])
            continue
        if stripped == "#else":
            stack[-1] = every - stack[-1]
            continue
        if stripped == "#endif":
            stack.pop()
            continue
        platforms = set(every)
        for condition in stack:
            platforms &= condition
        yield line, platforms


def _swift_platforms(condition: str) -> set[str]:
    if "os(iOS)" in condition:
        return {"ios"}
    if "os(macOS)" in condition:
        return {"macos"}
    return {"ios", "macos"}


def lane_facts(root: pathlib.Path) -> dict[str, dict[str, set[str]]]:
    """platform -> {"callbacks", "reports", "entries", "services"} its host layer wires."""
    facts = {
        platform: {
            "callbacks": set(), "reports": set(), "entries": set(), "services": set(), "builtin": set(),
            "events": set(),
        }
        for platform in PLATFORMS
    }

    def scan(platform_lines, files):
        for path in files:
            text = path.read_text(encoding="utf-8")
            lines = swift_conditions(text) if path.suffix == ".swift" else ((line, None) for line in text.splitlines())
            for line, only in lines:
                for platform in platform_lines if only is None else [p for p in platform_lines if p in only]:
                    for field in re.findall(r"callbacks\.(on_\w+)\s*=", line):
                        facts[platform]["callbacks"].add(field)
                    # The battery's status, and the network's: its whole report, or the
                    # status alone that is shorthand for one.
                    for status, report in re.findall(
                        r"migo_session_(?:set_(battery|network)_status|report_(network))\s*\(", line
                    ):
                        facts[platform]["reports"].add(status or report)
                    facts[platform]["entries"] |= set(re.findall(r"\b(migo_session_\w+)\s*\(", line))
                    if re.search(r"\bhost_services\s*=", line):
                        facts[platform]["services"].add("*")
                    # A service the host layer answers itself when the app does not
                    # (the Swift SDK's MigoBuiltInHostServices, the macOS game view's
                    # MigoGameViewHostServices, an optional capability product's
                    # services): `builtIn.insert(.name)`; in C++ (the Linux Host
                    # Kit's MigoQtBuiltInHostServices),
                    # `builtIn |= hostServiceBit(MIGO_HOST_SERVICE_NAME)`.
                    for name in re.findall(r"\bbuiltIn\.insert\(\.(\w+)\)", line.split("//", 1)[0]):
                        facts[platform]["builtin"].add(re.sub(r"(?<!^)([A-Z])", r"_\1", name).lower())
                    for name in re.findall(
                        r"\bbuiltIn\s*\|=\s*hostServiceBit\(MIGO_HOST_SERVICE_([A-Z_]+)\)", line.split("//", 1)[0]
                    ):
                        facts[platform]["builtin"].add(name.lower())
                    # A host-service event the host layer posts itself: Swift
                    # names it MigoHostServiceID.<Service>.Event.<event>, C and
                    # C++ MIGO_<SERVICE>_EVENT_<EVENT>.
                    code = line.split("//", 1)[0]
                    for service, event in re.findall(r"\bMigoHostServiceID\.(\w+)\.Event\.(\w+)", code):
                        snake = lambda word: re.sub(r"(?<!^)([A-Z])", r"_\1", word).lower()
                        facts[platform]["events"].add(f"{snake(service)}.{snake(event)}")
                    for service, event in re.findall(r"\bMIGO_([A-Z]+(?:_[A-Z]+)*?)_EVENT_([A-Z_]+)\b", code):
                        facts[platform]["events"].add(f"{service.lower()}.{event.lower()}")

    apple = found("Apple host layer sources", sorted((root / APPLE_LAYER).glob("*.swift")))
    scan(["ios", "macos"], apple)
    # Each game view answers what only its window can (MigoGameViewHostServices):
    # the Mac's window, the iPad's scene.
    mac_view = found("macOS game view sources", sorted((root / APPLE_MAC_VIEW).glob("*.swift")))
    scan(["macos"], mac_view)
    ios_view = found("iOS game view sources", sorted((root / APPLE_IOS_VIEW).glob("*.swift")))
    scan(["ios"], ios_view)
    for capability in APPLE_CAPABILITIES:
        scan(["ios", "macos"], found(f"{capability} sources", sorted((root / capability).glob("*.swift"))))
    linux = found("Linux host kit sources", sorted(p for p in (root / LINUX_LAYER).rglob("*") if p.suffix in (".cpp", ".hpp", ".h")))
    scan(["linux"], linux)
    ohos = found("OpenHarmony host sources", sorted(p for p in (root / OHOS_LAYER).rglob("*") if p.suffix in (".cpp", ".h", ".ets")))
    scan(["ohos"], ohos)
    for platform in ("ios", "macos", "linux", "ohos"):
        found(f"{platform} callbacks", facts[platform]["callbacks"])

    # The Swift session wraps every input entry point (MigoEngineSession.sendKey,
    # .sendPointer, ...), and both Apple platforms compile it, so its calls alone
    # say nothing about which platform feeds the input: an input is the platform's
    # only where code compiled for it calls the wrapper -- the platform's game view,
    # or the SDK's own drivers (gamepads, the device's reports). Without this the
    # iPhone and iPad were credited with the hardware keyboard and the mouse that
    # only the Mac's view delivers.
    input_entries = {entry for entry, _ in INPUT.values() if entry is not None}
    wrappers: dict[str, set[str]] = {}  # wrapper function -> input entry points it calls
    for path in apple:
        text = path.read_text(encoding="utf-8")
        for match in re.finditer(r"\bfunc\s+(\w+)\s*[(<]", text):
            body = braced(text, match.end())
            called = input_entries & set(re.findall(r"\b(migo_session_\w+)\s*\(", body))
            if called:
                wrappers.setdefault(match.group(1), set()).update(called)
    platform_sources = {
        "ios": apple + [p for c in APPLE_CAPABILITIES for p in sorted((root / c).glob("*.swift"))] + ios_view,
        "macos": apple + mac_view + [p for c in APPLE_CAPABILITIES for p in sorted((root / c).glob("*.swift"))],
    }
    for platform, sources in platform_sources.items():
        fed: set[str] = set()
        for path in sources:
            text = path.read_text(encoding="utf-8")
            compiled = "\n".join(line.split("//", 1)[0] for line, only in swift_conditions(text) if platform in only)
            for name, entries in wrappers.items():
                # A call, not the wrapper's own definition.
                if re.search(rf"(?<!func )\b{name}\s*\(", compiled):
                    fed |= entries
        facts[platform]["entries"] -= input_entries - fed
    return facts


# Where the Java SDK hands a call to the host app's handler. A call can reach the
# SDK through a service the SDK answers itself and be the handler's all the same:
# openSetting arrives through system_info and goes to the SettingHandler.
ANDROID_DELEGATE = re.compile(r'\bdelegate\(\s*\w+,\s*s(\w+)Handlers,\s*"(\w+)"')


def android_facts(root: pathlib.Path) -> tuple[set[str], set[str], set[str], dict[str, str]]:
    """(services the Android SDK implements, services left to a handler, input natives it calls,
    the API each call it hands to a handler is -> that handler's service)."""
    services_src = read(root, ANDROID_SERVICES)
    implemented: set[str] = set()
    for block in re.finditer(r"impl \w+ for AndroidDeviceServices \{", services_src):
        implemented |= set(re.findall(r"fn (\w+)\(&self\) -> Option<Arc<dyn", braced(services_src, block.start())))
    found("Android services", implemented)
    handlers = set()
    for service, interface in ANDROID_HANDLER.items():
        if not (root / ANDROID_HANDLERS / f"{interface}.java").exists():
            raise Fail(f"Android's {service} handler {interface}.java is gone")
        handlers.add(service)
    natives = set()
    for path in (root / ANDROID_JAVA).rglob("*.java"):
        if path.name in ("NativeBridge.java", "NativeMethods.java"):
            continue
        # A call, or a method reference handed to whatever calls it.
        for call, reference in re.findall(
            r"\bNative(?:Methods|Bridge)(?:\.(on\w+)\s*\(|::(on\w+)\b)", path.read_text(encoding="utf-8")
        ):
            natives.add(call or reference)
    by_interface = {interface: service for service, interface in ANDROID_HANDLER.items()}
    delegated = {}
    for handler, api in ANDROID_DELEGATE.findall(read(root, f"{ANDROID_JAVA}/com/migo/runtime/internal/NativeExports.java")):
        if f"{handler}Handler" not in by_interface:
            raise Fail(f"Android delegates {api} to {handler}Handler, which ANDROID_HANDLER does not name")
        delegated[api] = by_interface[f"{handler}Handler"]
    return implemented, handlers, found("Android input natives", natives), found("Android delegated calls", delegated)


# ---- what each API needs -------------------------------------------------------


def needs_of(name, reach, reference, decisions, channel, capi, memo, stack=()):
    if name in memo:
        return memo[name]
    if name in stack:
        raise Fail(f"via cycle: {' -> '.join(stack + (name,))}")
    entry = reach[name]
    via = decisions["via"].get(name)
    if via is not None:
        if via not in reach or reach[via] is None:
            raise Fail(f"{name} is via {via}, which is not a published API")
        needs = needs_of(via, reach, reference, decisions, channel, capi, memo, stack + (name,))
        memo[name] = needs
        return needs
    if name in decisions["input"]:
        needs = {f"input:{decisions['input'][name]}"}
        memo[name] = needs
        return needs
    if name in decisions["events"]:
        needs = {f"event:{decisions['events'][name]}"}
        memo[name] = needs
        return needs
    ops, services = set(entry["ops"]), set(entry["services"])
    if name.startswith(("on", "off")) and not services and name not in decisions["pure"]:
        siblings = [
            other for other, value in reach.items()
            if value and reference[other] == reference[name] and not other.startswith(("on", "off"))
        ]
        for other in siblings:
            ops |= set(reach[other]["ops"])
            services |= set(reach[other]["services"])
        if not services and not ops:
            raise Fail(f"{name} reaches nothing and has no API of its kind to follow; decide it (pure, via or input)")
    if not ops and not services and name not in decisions["pure"]:
        raise Fail(
            f"{name} reaches nothing: give the probe a call that reaches its host "
            "(engine/crates/runtime-v8/src/tests/api_reach.rs), or decide it in api-platform-decisions.json"
        )
    needs = set()
    channel_services = set()
    for op in ops:
        if op in channel:
            service, method = channel[op]
            needs.add(f"channel:{service}.{method}")
            channel_services.add(service)
    for accessor in services:
        kind, detail = capi.get(accessor, ("absent", None))
        if kind == "channel":
            if detail not in channel_services:
                needs.add(f"channel:{detail}")
        elif kind == "mixed":
            service, field = detail.split("|")
            if service not in channel_services:
                needs.add(f"callback:{field}")
        elif kind == "callback":
            needs.add(f"callback:{detail}")
        elif kind == "report":
            needs.add(f"report:{detail}")
        elif kind == "native":
            needs.add(f"native:{accessor}({detail})")
        elif kind == "absent":
            needs.add(f"service:{accessor}")
    memo[name] = needs
    return needs


def accessors_of(name, reach, reference, decisions):
    """The engine services an API asks for, following via and its kind's siblings."""
    if name in decisions["events"]:
        return set()
    seen = set()
    while name in decisions["via"]:
        if name in seen:
            break
        seen.add(name)
        name = decisions["via"][name]
    entry = reach[name]
    services = set(entry["services"])
    if name.startswith(("on", "off")) and not services and name not in decisions["pure"] and name not in decisions["input"]:
        for other, value in reach.items():
            if value and reference[other] == reference[name] and not other.startswith(("on", "off")):
                services |= set(value["services"])
    return services


def ops_of(name, reach, reference, decisions):
    """The ops an API dispatches, following via and its kind's siblings -- the
    same rule its needs are derived by."""
    if name in decisions["events"]:
        return set()
    seen = set()
    while name in decisions["via"] and name not in seen:
        seen.add(name)
        name = decisions["via"][name]
    entry = reach[name]
    ops = set(entry["ops"])
    # Removing a listener reaches what adding it does, and nothing else.
    if name.startswith("off") and not ops and not entry["services"]:
        paired = "on" + name[3:]
        if reach.get(paired) is not None:
            return ops_of(paired, reach, reference, decisions)
    if (
        name.startswith(("on", "off"))
        and not entry["services"]
        and name not in decisions["pure"]
        and name not in decisions["input"]
    ):
        for other, value in reach.items():
            if value and reference[other] == reference[name] and not other.startswith(("on", "off")):
                ops |= set(value["ops"])
    return ops


# ---- per platform --------------------------------------------------------------


def worst(statuses) -> str:
    statuses = list(statuses)
    return min(statuses, key=RANK.index) if statuses else "engine"


def need_status(need: str, platform: str, lanes, entries, android) -> str:
    kind, _, detail = need.partition(":")
    implemented, handlers, natives, _ = android
    if platform == "android":
        if kind == "input" and INPUT[detail][1] is not None:
            return "default" if INPUT[detail][1] in natives else "missing"
        if kind == "event":
            return "default" if ANDROID_EVENTS.get(detail) in natives else "missing"
        return None  # decided by the accessor, below
    lane = lanes.get(platform)
    if kind == "service":
        return "missing"
    if kind == "native":
        # The engine's own implementation, on the operating systems it is
        # compiled for.
        return "default" if platform in detail.split("(", 1)[1].rstrip(")").split(",") else "missing"
    if kind == "input":
        entry = INPUT[detail][0]
        if entry is None or entry not in entries:
            return "missing"
        if lane is not None and entry in lane["entries"]:
            return "default"
        return "host" if platform in ("linux", "windows") else "missing"
    if kind == "callback":
        if lane is not None and detail in lane["callbacks"]:
            return "default"
        return "host" if platform in ("linux", "windows") else "missing"
    if kind == "report":
        if lane is not None and detail in lane["reports"]:
            return "default"
        return "host" if platform in ("linux", "windows") else "missing"
    if kind == "event":
        if lane is not None and detail in lane["events"]:
            return "default"
        if lane is not None and "*" in lane["services"]:
            return "host"
        return "host" if platform in ("linux", "windows") else "missing"
    if kind == "channel":
        if lane is not None and detail.split(".")[0] in lane["builtin"]:
            return "default"
        if lane is not None and "*" in lane["services"]:
            return "host"
        return "host" if platform in ("linux", "windows") else "missing"
    raise Fail(f"unknown need {need}")


def owned(need: str, host_owned: set[str]) -> bool:
    kind, _, detail = need.partition(":")
    if kind == "channel":
        return detail.split(".")[0] in host_owned
    if kind == "callback":
        return detail in {f"on_{name}" for name in host_owned}
    return False


def offered(clients: dict | None, platform: str) -> bool:
    """Whether the reference offers an API on the kind of client `platform` is held to."""
    clients = clients or {}
    for client in REFERENCE_CLIENTS[platform]:
        if client == "mobile":
            if clients.get("mobile", True) is not False:
                return True
        elif clients.get(client, False) not in (False, None):
            return True
    return False


def generate(root: pathlib.Path) -> str:
    reference = load(root, REFERENCE)["apis"]
    spec = load(root, SPEC)["apis"]
    if set(spec) != set(reference):
        raise Fail("minigame-api-spec.json does not cover the reference's list; regenerate it")
    reach = load(root, REACH)["apis"]
    decisions = load(root, DECISIONS)
    boundary = load(root, OP_BOUNDARY)
    if set(reach) != set(reference):
        missing = sorted(set(reference) - set(reach))
        extra = sorted(set(reach) - set(reference))
        raise Fail(f"the reach record is not the reference's list (missing {missing[:5]}, extra {extra[:5]}); rerun the probe")
    decisions.setdefault("events", {})
    host_events = {
        f"{service}.{event}"
        for service, entry in load(root, HOST_SERVICES)["services"].items()
        for event in entry.get("events", {})
    }
    for name, event in decisions["events"].items():
        if event not in host_events:
            raise Fail(f"events decides {name} by {event}, which is no host-service event")
    for section in ("pure", "via", "input", "events", "gaps", "unsupported"):
        for name in decisions[section]:
            if name not in reference:
                raise Fail(f"{section} decides {name}, which is not in the platform's API")
    for section in ("gaps", "unsupported"):
        for name, decision in decisions[section].items():
            platforms = decision["platforms"]
            if platforms != "*" and not set(platforms) <= set(PLATFORMS):
                raise Fail(f"{section}.{name} names an unknown platform")
            if not decision.get("reason"):
                raise Fail(f"{section}.{name} gives no reason")
    for name in decisions["unsupported"]:
        if reach[name] is not None and decisions["unsupported"][name]["platforms"] == "*":
            raise Fail(f"{name} is unsupported everywhere but published; unpublish it or narrow the decision")
    host_owned = set(decisions["host_owned"])

    channel = channel_ops(root)
    capi = capi_services(root)
    entries = c_entry_points(root)
    lanes = lane_facts(root)
    android = android_facts(root)
    implemented, handlers, natives, delegated = android
    # An op of a call the SDK hands to a handler -> that handler's service.
    handed_ops = {}
    for api, service in delegated.items():
        if reach.get(api) is None:
            raise Fail(f"Android delegates {api}, which is not a published API")
        for op in reach[api]["ops"]:
            handed_ops[op] = service
    # The input table names real entry points: one it misspells reads as no
    # platform feeding that input, which is how touch was missing everywhere.
    bridge = (root / ANDROID_JAVA / "com/migo/runtime/internal/NativeMethods.java").read_text(encoding="utf-8")
    bridge += (root / ANDROID_JAVA / "com/migo/runtime/internal/NativeBridge.java").read_text(encoding="utf-8")
    for kind, (entry, native) in INPUT.items():
        if entry is not None and entry not in entries:
            raise Fail(f"input {kind}: {entry} is not a C ABI entry point")
        if native is not None and not re.search(rf"\bstatic\b[^;{{]*\b{native}\s*\(", bridge):
            raise Fail(f"input {kind}: {native} is not a method of the Android SDK's native bridge")
    p_lanes = {}
    for extension in boundary["extensions"].values():
        for op, lane in extension["ops"].items():
            p_lanes[op] = lane if isinstance(lane, str) else lane.get("lane", "")

    def covers(decision, platform):
        return decision["platforms"] == "*" or platform in decision["platforms"]

    measured = conformance(root, reference)
    memo: dict = {}
    rows = {}
    summary = {platform: collections.Counter() for platform in PLATFORMS}
    for name in sorted(reference):
        row = {"category": reference[name]}
        statuses = {}
        verified = []
        not_ready = []
        outside = []
        decided = [
            decision for decision in (decisions["gaps"].get(name), decisions["unsupported"].get(name))
            if decision is not None
        ]
        if reach[name] is None:
            needs = set()
            row["needs"] = ["unpublished"]
        elif all(any(covers(decision, platform) for decision in decided) for platform in PLATFORMS):
            # Decided on every platform: what it would need is not the question yet.
            needs = set()
            row["needs"] = []
        else:
            needs = needs_of(name, reach, reference, decisions, channel, capi, memo)
            row["needs"] = sorted(needs)
        handed = (
            {handed_ops[op] for op in ops_of(name, reach, reference, decisions) if op in handed_ops}
            if reach[name] is not None
            else set()
        )
        for platform in PLATFORMS:
            unsupported = decisions["unsupported"].get(name)
            gap = decisions["gaps"].get(name)
            if unsupported and covers(unsupported, platform):
                status = "unsupported"
            elif reach[name] is None or (gap and covers(gap, platform)):
                status = "missing"
            elif platform == "android":
                parts = []
                for accessor in accessors_of(name, reach, reference, decisions):
                    if accessor not in implemented:
                        parts.append("missing")
                    elif accessor in handlers:
                        parts.append("host")
                    else:
                        parts.append("default")
                if handed:
                    parts.append("host")
                for need in needs:
                    status = (
                        need_status(need, platform, lanes, entries, android)
                        if need.startswith(("input:", "event:"))
                        else None
                    )
                    if status is not None:
                        parts.append(status)
                status = worst(parts)
            else:
                status = worst(need_status(need, platform, lanes, entries, android) for need in needs)
                if platform == "ios" and status != "missing":
                    lanes_of_ops = [p_lanes.get(op) for op in ops_of(name, reach, reference, decisions)]
                    # iOS runs content on the Performance+ producer: an op it does
                    # not offer reaches nothing, and one it answers itself
                    # (`local`) never reaches the host's channel or what the host
                    # reports, whatever the host supplies.
                    if "unsupported" in lanes_of_ops or (
                        "local" in lanes_of_ops
                        and any(need.startswith(("channel:", "input:", "report:")) for need in needs)
                    ):
                        status = "missing"
            statuses[platform] = status
            wired = status in ("engine", "default", "unsupported") or (
                status == "host"
                and all(
                    owned(need, host_owned) or need_status(need, platform, lanes, entries, android) != "host"
                    for need in needs
                )
                and (
                    platform != "android"
                    or all(
                        accessor not in handlers or accessor in host_owned
                        for accessor in accessors_of(name, reach, reference, decisions) | handed
                    )
                )
            )
            # Wired is not ready: a cell is ready where its API was verified on
            # the platform -- or is deliberately not offered, or is the host
            # app's to answer, which no platform verifies for it.
            is_verified = measured.get(platform, {}).get(name) == "verified"
            if is_verified:
                verified.append(platform)
                summary[platform]["verified"] += 1
            ready = wired and (is_verified or status in ("unsupported", "host"))
            if not offered(spec[name].get("clients"), platform):
                outside.append(platform)
                summary[platform]["outside"] += 1
                ready = True
            summary[platform][status] += 1
            if wired:
                summary[platform]["wired"] += 1
            if ready:
                summary[platform]["ready"] += 1
            else:
                not_ready.append(platform)
        row.update(statuses)
        if verified:
            row["verified"] = verified
        if outside:
            row["outside"] = outside
        if not_ready:
            row["not_ready"] = not_ready
        rows[name] = row
    return render(rows, summary, len(reference))


def conformance(root: pathlib.Path, reference: dict) -> dict[str, dict[str, str]]:
    """platform -> API -> its measured status, for each platform a record exists for."""
    measured = {}
    for platform in PLATFORMS:
        path = root / CONFORMANCE.format(platform=platform)
        if not path.exists():
            continue
        record = json.loads(path.read_text(encoding="utf-8"))
        if record.get("platform") != platform:
            raise Fail(f"{path.name} is a record of {record.get('platform')!r}, not {platform}")
        apis = record["apis"]
        if set(apis) != set(reference):
            missing = sorted(set(reference) - set(apis))
            extra = sorted(set(apis) - set(reference))
            raise Fail(f"{path.name} does not measure the platform's API list (missing {missing[:5]}, extra {extra[:5]})")
        for name, entry in apis.items():
            if entry.get("status") not in CONFORMANCE_STATUSES:
                raise Fail(f"{path.name}: {name} is {entry.get('status')!r}, not one of {CONFORMANCE_STATUSES}")
        measured[platform] = {name: entry["status"] for name, entry in apis.items()}
    return measured


def render(rows, summary, total) -> str:
    out = ['{', '  "schema_version": 1,', '  "$comment": ' + json.dumps(COMMENT, indent=4).replace("\n", "\n  ") + ",",
           f'  "platforms": {json.dumps(list(PLATFORMS))},', f'  "total": {total},', '  "summary": {']
    for index, platform in enumerate(PLATFORMS):
        counts = {
            key: summary[platform][key]
            for key in ("engine", "default", "host", "missing", "unsupported", "wired", "verified", "outside", "ready")
        }
        out.append(f'    "{platform}": {json.dumps(counts)}' + ("," if index + 1 < len(PLATFORMS) else ""))
    out.append("  },")
    out.append('  "apis": {')
    names = sorted(rows)
    for index, name in enumerate(names):
        out.append(f"    {json.dumps(name)}: {json.dumps(rows[name], ensure_ascii=False)}" + ("," if index + 1 < len(names) else ""))
    out.append("  }")
    out.append("}")
    return "\n".join(out) + "\n"


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=pathlib.Path, default=pathlib.Path(__file__).resolve().parent.parent)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args(argv)
    text = generate(args.root)
    target = args.root / MATRIX
    if args.check:
        if not target.exists() or target.read_text(encoding="utf-8") != text:
            print(f"{MATRIX} is stale: regenerate it with scripts/gen-api-platform-matrix.py and review the diff", file=sys.stderr)
            return 1
        print(f"{MATRIX} matches what it is derived from")
        return 0
    target.write_text(text, encoding="utf-8")
    data = json.loads(text)
    for platform in PLATFORMS:
        counts = data["summary"][platform]
        print(
            f"{platform:8} ready {counts['ready']:3}/{data['total']}  "
            + "  ".join(
                f"{k} {counts[k]}"
                for k in ("wired", "verified", "outside", "engine", "default", "host", "missing", "unsupported")
            )
        )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
