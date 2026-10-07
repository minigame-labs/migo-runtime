#!/usr/bin/env python3
"""Select an OpenHarmony sysroot that is strictly newer than the floor SDK.

Usage: select-ohos-newer-sysroot.py <floor-sdk-home>

Prints the path of a newer SDK's ``native/sysroot`` and exits 0. Prints nothing
and exits 0 when no candidate qualifies -- "none installed" is a normal, honest
outcome that the caller reports rather than a failure: a machine without a
second SDK must report the reduced check, not fail as though the change under
test broke something.

WHY THE CHOICE IS ON apiVersion AND NOT ON THE DIRECTORY NAME
------------------------------------------------------------
A directory called ``ohos-sdk-6.1`` is just a name, the same reason the NDK pin
does not trust one. Taking the highest-sorted non-floor directory looks
equivalent and is not: if the floor happens to be the newest SDK installed, that
rule hands back an *older* sysroot and the gate then reports its extra symbols
as "post-floor" -- evidence pointing the wrong way, and pointing it at a passing
verdict. A candidate therefore has to *declare* a higher API than the floor.

This lives in its own file, rather than in a heredoc inside
``build-ohos-sdk.sh``, because an independent review observed that the wrong
first draft above was reintroducible with no gate firing. Logic embedded in a
shell heredoc cannot be exercised directly, so it had only ever been checked by
hand. ``scripts/test-ohos-newer-sysroot-selection.sh`` now covers the three
layouts that distinguish the rules.
"""

import json
import pathlib
import sys


def api_of(home: pathlib.Path):
    """The API level an SDK declares about itself, or None if it declares none."""
    described = home / "native/oh-uni-package.json"
    try:
        return int(json.loads(described.read_text(encoding="utf-8"))["apiVersion"])
    except (OSError, ValueError, KeyError):
        return None


def select(floor_home: pathlib.Path):
    """The newest sibling SDK declaring a higher API than the floor, or None."""
    floor_api = api_of(floor_home)
    if floor_api is None:
        # The floor does not say what it is, so "newer than the floor" has no
        # meaning and no candidate can be justified against it.
        return None
    best_api, best = floor_api, None
    for candidate in sorted(floor_home.parent.glob("ohos-sdk*")):
        if not (candidate / "native/sysroot").is_dir() or candidate == floor_home:
            continue
        api = api_of(candidate)
        if api is not None and api > best_api:
            best_api, best = api, candidate
    return best


def main(argv):
    if len(argv) != 2:
        print(f"usage: {argv[0]} <floor-sdk-home>", file=sys.stderr)
        return 2
    selected = select(pathlib.Path(argv[1]))
    if selected is not None:
        print(selected / "native/sysroot")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
